# DSP SDK and first sampler

## Current product requirements — corrected 2026-10-30

The first sampler requires full stereo on both DT2 OS 1.17 and DN2 OS 1.12,
across 16 tracks sharing one project sample pool. Do not divide capacity into fixed
per-track partitions. References to the same asset should share its backing through
deduplication. Long samples are limited by real pool capacity and supported format
limits, not a ten-second cap. Stock DT2-like project capacity is the target, not
proven parity on either device.

Keep one shared SDK/sampler engine and runtime-shipped device adapters. Selecting a
sample stops only the selected track's old notes. The on-device picker remains a
first-release requirement. The payoff is useful stereo sampling across the project,
with shared accounting rather than a small allocation multiplied by track count.
Confidence in the shared architecture remains high; achievable capacity, stereo
representation and production backing ownership remain unproved.

Mono banks and ten-second clips below describe historical prototype scope and proof
fixtures, not current product limits. Historical empirical results remain valid only
within their recorded scope. The proposed 16 MiB DT2 tail recommendation is withdrawn;
there is no outstanding reservation-choice request and no approved reservation patch.


## Latest checkpoint

**Checkpoint (2026-11-01):**

- Main is merged into the branch (`bd12a6bf`). `approx_recips` is removed and the
  sampler findings are renumbered 104-107.
- Cache visibility is answered statically ([finding 108](../findings/108-dt2-sample-cache-visibility.md)).
- DT2 backing: borrow the stock sample pool through a `core/adapters/sample-pool`
  adapter. This is emulator-confirmed ([finding 109](../findings/109-dt2-stock-pool-borrowing.md)).
- DN2 backing: reserved high DDR. The canary soak saw no stock writes in
  `[0x80531000, 0xa0000000)` in 20 scenarios
  ([finding 106](../findings/106-dn2-ddr-configuration.md#canary-map-of-stock-ddr-writes-2026-11-01)).
- DN2 stereo: the shadow stages go to production. The two-slot route is rejected
  ([finding 96](../findings/96-stereo-machine-processing.md)).
- Project identity: [finding 110](../findings/110-project-identity.md).
- API 1.5 accepted by Em (2026-11-01): [sample API 1.5](../design/sample-api-1.5.md)
  (stereo descriptor, per-track binding, device-neutral runtime plus a
  core/adapters/sample-pool backing adapter), with the recommendations in its section 5.
- Re-proof after the merge (2026-11-01): `test_dsp_sample_stereo_kernels.py` and
  `test_dsp_sample_bank.py` pass (61). `sample-ddr-transfer` (12/12, both
  devices), `file-catalog` and `stock-pool-borrow` pass on daemons built from the
  merged tree. `sample-playback.test.ts` fails on both devices with
  `log: E_PROTO`: main (`985aab04`) made the log commands exist only while
  `digikit/debug` is loaded, and the test does not select it (`live.ts` has the
  helper). Fix in the build phase.
- Working rule for the design phase: static work, docs and design only. Run the
  emulator only when the run decides a design question.
- First slice proven on DT2 in the emulator (`shot-dt2.device.test.ts`): API 1.5
  select in about 0.7-0.8 s, borrowed pool, stop-old-notes on reselect, stock
  reset detected and reloaded with a new token, contract 1.2 stereo split via
  the pair gate ([finding 112](../findings/112-dt2-pair-gate.md)).
- Open: the pitch phase (the PITCH knob has no effect; being investigated), the
  gate anchor, the finding 111 decision (fix the stereo kernel or keep SHOT's
  double-mono workaround), the DN2 stereo cost, the picker (slice 2).
- Proposed, not yet decided by Em: the minimal first-release recovery scope.

All ten DN2 post-source families have bounded local examples; the selected ordered
replay has 44 cases. The opt-in first-order/U/Y observer passes 75 model cases and
four native cases: identity/polarity in Pre and a legitimate held-note BW.RT
Pre→Post panel switch. Stock/patched stereo WAVs match exactly. The paired
interpreter route profile positively observes both U/Y sites (early 10,160,
late 9,040 each), closing the previously open late-placement scenario gap.
These are two private stages and one held-note route, not full stock integration.
See [finding 96](../findings/96-stereo-machine-processing.md).

The private stereo-frame fetch proof and its reusable eight-instruction raw SDK
fetch macro pass 59 model cases. Actual DDR aliases, whole packed-word footprints
and original guard stores are covered; exclusion/callback-lifetime policy remains
private. The DDR diagnostic has compiled-native, ordered-RPC and bounded Wasm
proofs. See [finding 99](../findings/99-stereo-frame-fetch-exclusion.md) and
[finding 97](../findings/97-ddr-access-diagnostic.md).

Runtime failed-load cleanup now retains DSP/CF/module/transfer storage on uncertain
invalidation and stops the queue. Ten actual-runtime host failure/success tests and
two-device normal pluck load acceptance pass; standalone dkrt tests now route
through the existing Rust job and `test:runtime:unit` owner. This does not prove
physical ACK/callback quiescence. See [finding 99](../findings/99-stereo-frame-fetch-exclusion.md).

The read-only `fw stores` report passes 25 named tests and independent review.
Its DN2 requested PC `0x1c9fe7` is now byte-audited as the actual AudioTask entry;
merged database owner `0x1c9e76` makes whole-function reach an overapproximation.
The bounded report has 93 functions/2,278 recorded stores, not ownership closure.
All 23 CALL rows missing target functions have concrete PCs; the one with no
containing range, `0x1c125e`, now has a bounded store-free body audit. Its caller's
R4 provenance closes the `0xb80e79` store as per-slot voice-state word 2, separate
from external ring backing. Auxiliary `0x1ca59f` setup now resolves SPI1 TX/RX
fixed roots and two-byte count/modify configuration; XCNT=0 is an error, not an
inactive/empty transfer. The bounded static lifecycle audit links the published
auxiliary handle to callback shutdown/control; creator invocation/N and complete
error-path behavior remain unresolved. Next resolve external/computed creator
entry or the capability for exact-entry R4 observation. External ring ownership, entry/dispatch
precision remain open. The DT2 reserve/alternate-reclaim audit confirms that
write/delete-only hooks miss allocation and multi-ID reclaim. The create/ID-reuse
slice now identifies pre-mutation ID and incarnation-like node metadata without
wrap rejection. Bounded format/mount/init and successful blocking-I/O audits now
identify distinct lifecycle boundaries; shared init is too late and mounted state
is not an epoch. The bounded error audit finds event signaling and early returns
without local abort/drain, not physical quiescence. The pointer audit now shows
DMA into stock cache/bounce storage, followed by a caller copy, with no direct
caller-buffer branch or after-free-write evidence. Ignored I/O errors can still
produce stale/incomplete cache copies with success counts. The next design
requirement is lifecycle/content-change evidence plus independently latched
low-level storage faults; recovery mechanisms, full hook coverage and source
consistency remain open, with no implementation selected. No hooks or complete
generation scheme are selected. See
[finding 106](../findings/106-dn2-ddr-configuration.md) and
[finding 81](../findings/81-stock-sampler-dependencies.md).

**Accepted first-release recovery policy:** missing, invalid or changed samples
show an error and discard the unpublished load; the selected track stays silent,
and another selection is allowed. An uncertain low-level storage fault with
unproved DMA completion latches the fault and blocks new sample loads until reboot
or independently proven recovery. The loader leaves other published bindings alone,
without guaranteeing underlying-device health. Automatic retry/recovery is not
required for the first release. Next technical work is correct low-level fault
capture/history plus a lifecycle/content-change source witness before cache use.
The accepted policy does not establish implementation completeness, DMA stopping,
hook coverage or production backing/reservation approval.

The early DT2 MAIN audit now places patched-in segment BSS zeroing before the
traced storage ISR installation, first command and later FS/runtime task startup.
An optional private patched-in DT2 observer now has compiled model and static
image/loader evidence **[D]**, plus bounded native zeroing/host-seed retention
observations. Native IRQ capture remains open: the default Oracle completion path
posts storage semaphores directly, and the run observed no observer stores.
Controlled actual ISR/hook replay now passes, including error plus completion;
real delivery, complete initialization/reset history and source witnesses remain
open. This is not production gating or backing approval.

Production backing ownership/coexistence, callback-private lifetime, full-route
capture/commit/reset/cleanup, public stereo integration and picker publication
remain open. No production reservation or new IRQ policy is selected. Hardware,
40-bit/exceptional correctness and performance claims remain outside these proofs.

## Historical checkpoint — LP0 0.5 startup window and real readers passed

LP0 0.5 passes the bounded DN2 1.12 native actual-reader proof with fresh cache
register metadata and a boot-configured private RR4 window. Both tracks start,
CLOSE A drains without injected counts, A emits fresh raw silence, and B continues
its raw waveform and audible post-drain tone. All 101 owner tests pass. Independent
source, original-byte and artifact reviews are clean. The same run also restores
stock and then the patched ready-pair cache, preserving the initialized window.

Successful run: `out/runs/lp0-proof/b734a462-c9b0-4a67-a5a4-e0982c845d27/`.
Fixture: `out/runs/lp0-cache-v5-fixture/`; command/log: `out/runs/lp0-cache-v5-run3/`.
Finding 95 records the new evidence and a coupled-startup bug found and fixed:
changed DSP firmware now initializes from raw reset state with its patch applied
once, before ColdFire starts. No hardware work or instruction-core regeneration
was performed; other sessions' processes were left untouched.

Next: establish production backing ownership/reset fences before exposing per-track
binding and connecting the on-device picker. The private staging window is not a
production DDR reservation. DT2 loader/reset and DN2 runtime-ring evidence remains
in findings 81/91. Physical cache behavior, broader firmware coexistence, and the
existing AudioTask-tail SIMD save limitation remain open. The new runtime path has
native DN2 acceptance and DT2/second-core startup/cache integration acceptance;
the full merge gate remains required before merging.

## Confirmed cross-device architecture

The user confirmed one shared sampler/SDK engine for both DT2 and DN2, with
separate adapters only where device memory acquisition, transport, storage or
project/reset integration differ. Both adapters must provide the same ready
sample view and reader-lifetime contract. DT2 stock sampling is a reference and
potential backing implementation, not a dependency imposed on DN2.

The user confirmed that device adapters ship with the runtime. Content modules
depend only on the shared SDK and declare the required contract/version; a
compatible runtime supplies the device implementation and rejects unsupported
requirements. Modules must not separately install a device adapter. Extend the
existing runtime compatibility mechanism rather than inventing a parallel one.

The earlier 16 MiB DT2 tail proposal is withdrawn. Investigate shared project-pool
allocation, deduplication and lifecycle integration against the current stereo and
capacity requirements. No reservation patch is approved. DN2 already has the historical
ten-second actual-reader proof; production ownership and capacity remain distinct
from that bounded playback result.

## Current bounded discovery — production ownership

- [x] Reject the hidden recoverable L1 allocator hypothesis using original bytes.
- [x] Identify a stock L1 cache-configuration helper that avoids address conversion.
- [x] Implement and prove a private whole-window RR4 startup diagnostic.
- [x] Trace DT2 project-pool capacity/reset paths; one size literal is not a
  complete reservation boundary.
- [ ] Reconcile a finite owner/boundary/reset/bypass table with DN2's fixed-buffer
  map to establish production ownership. Absence of discovered writers does not
  establish ownership.
- [~] Inventory stereo file/descriptor formats, channel layout, frame/length fields
  and arithmetic limits on both devices, including long-sample boundary cases.
  Partial: DT2 evidence in finding 81.
- [~] Establish DT2 stereo allocation accounting: bytes per frame, guards/alignment,
  slot/reference deduplication and actual shared project-pool capacity.
- [ ] Establish DN2 backing ownership, usable capacity and reset/coexistence bounds
  for the same shared-pool contract, without fixed per-track partitions.

The payoff is a concrete adapter ownership boundary and capacity accounting for
full-stereo, long samples shared across 16 tracks. Stock DT2-like capacity is a
target, not established parity. Confidence remains moderate; production memory
ownership is not selected.
Limit further investigation to named boundary gaps rather than arbitrary computed
stores. Hardware testing still requires a separate decision.

## Rebase checkpoint — main 409414f9

The sampler branch was rebased onto `origin/main` at `409414f9`. Original tip
`68db1215` is retained by `backup/dsp-sampler-pre-rebase-68db1215`. The inherited
cleanup WIP `219570ca` was skipped after independent comparison confirmed that
main's completed self-contained-module migration supersedes it. All 42 sampler
commits were retained. Historical commit hashes and run evidence below identify
their original checkpoints.

- [x] Preserve main fault tracing, diagnostics, trace-aware build cache and SHARC
  legality checks alongside sample/file APIs and source-track callbacks.
- [x] Fix two build API integration mismatches and update stale encoded DKM
  contract expectations (API 1.4, machine 1.1); independent review is clean.
- [x] Rebuild runtime and both voice objects from merged sources. Refresh DDR
  proof source hashes without changing its object bytes; no temporary conflict
  objects remain. SDK/FW/module generation and selected current-object checks pass.
- [x] Validate 103 SDK tests; cache-generation 1, header 1, ABI-generation 4,
  SDK-generation 8, trace 1, voice guard/track contract 17 and breadcrumb 4 tests.
  LP0 and DDR proof builds pass the new hardware-loop legality check.
- [x] Reapply private LP0 0.3 with the new patchkit: exact firmware image remains
  `6ff3b5ec9d9ed80f1108746922a213a75c9827a867fb257c1690c419fa56a2c8`.
  Its prior native pass still applies to those bytes; no redundant native run or
  emulator core regeneration was performed. This is not a new merged-runtime
  device acceptance result or full merge gate.

Routed `test:changed` now passes path and layer checks; its host/format/types
launchers retain the known linked-pnpm failure. Direct installed-tool fallbacks
pass: 222 host tests (16 skipped), all five workspace TypeScript checks and scoped
Prettier. No dependency files or installation were changed. Logs:
`out/rebase-changed-check.log`, `out/rebase-host-check.log`.

Main's DSP-link full-cache code-load sequence is useful evidence, but does not
settle DMA receive coherence or production sample-pool ownership. These remain
before exposing usable per-track bindings; see finding 95. Continue the sampler
work from the reader/backing integration boundary below.

Status: discovery and the first implementation milestone approved by Em on 2026-10-29.
Approved implementation: exact reusable DSP extractions, numerical sampler primitives, and the bounded
loading/lifetime proof below (Em approved 2026-10-29). The product sampler remains a separate decision.

## Resume here

**First-release UX decided (2026-10-30): on-device sample selection.**
Em requires an on-device picker in the initial sampler and points to the existing
SDK UI components. Workbench remains the transfer tool; per-track assignment must
be usable on the device. Reuse `sdk/include/dk/ui.h` and `core/ui` lists, actions,
side panels and dialogs rather than introducing a separate UI framework.

- [x] Resolve the on-device-versus-Workbench selection decision.
- [x] Locate existing `DK_UI_LIST`, fill/refresh callbacks and module menu entry.
- [x] Confirm listing belongs in the reusable SDK for a later file manager,
  not inside the sampler (Em, 2026-10-30).
- [x] Implement the generic DN2 store catalogue backend with bounded pages,
  owned full paths/metadata and sticky stale-generation errors; focused checks pass.
- [x] Independently audit the DT2 directory iterator's plain-data state/output ABI.
- [x] Prove DT2 path opening, iteration, type/size mapping, EOF and errors in the
  live emulator; independently audit the stock metadata/hash getter.
- [x] Map DN2 store and DT2 +Drive catalogue providers and identity metadata;
  neither listing establishes an immutable file lease.
- [x] Implement and independently review fixed-memory DN2 directory projection:
  implied/empty folders, sorted continuation pages and exact file metadata.
- [x] Make initial store verification incremental and retain generation on rewind;
  independent review and all 80 SDK host tests pass.
- [x] Test/review the shared listing request/cancel/notification lifecycle model;
  all 93 lifecycle/helper SDK tests pass.
- [x] Define/export the reusable SDK file/directory listing contract and adapters.
- [x] Integrate runtime serialization, deferred dispatch and loader retirement;
  public API 1.4 / runtime 0.5.4, independent source review and 94 SDK tests pass.
- [x] Prove the public C boundary on DN2 1.12 and DT2 1.17: both roots,
  directory/file rows and typed metadata, copied-row lifetime, stale tickets,
  invalid-capacity retention and reentrant callback cancellation, without slot 0.
- [ ] Define per-track selection/binding separately, preserving the existing
  pinned-bank proof API until a versioned replacement is available.
- [x] Specify per-track selection ordering and per-callback reader retirement;
  independent design review passes. No new public binding ABI is exported.
- [x] Prove a private two-track descriptor/retirement boundary in the assembled
  consumer: 15 focused tests and independent review pass. Physical IRQ/DMA and
  production transport/backing remain separate obligations.
- [x] Bound the private DN2 single-descriptor STOP receive ownership window;
  synchronous SEC dispatch and nested-interrupt limits are recorded in finding 95.
- [x] Isolate the private RX buffer's complete cache-line footprint; 47 focused
  cases, rebuilt module object and independent review pass (fixture 0.2.1).
- [x] Replaced: no cache-maintenance path is needed. DT2 is answered by
  [finding 108](../findings/108-dt2-sample-cache-visibility.md); DN2 uses the RR4
  non-cacheable window.
- [x] Implement bounded catalogue access with focused tests.
- [ ] Implement per-track binding with focused tests.
- [ ] Build the on-device picker using SDK UI components, with loading/error states.
- [ ] Integrate owned backing and prove selection/replacement/playback on both devices.

Picker design direction: capture the intended track when opening; browsing does
not retire a sample. Confirming selection stops only that track's old notes, queues
loading, and publishes the new generation only after validation and DSP completion.
Keep callback context in persistent module memory, page catalogue rows, and use
stable sample identity rather than a truncated display name or volatile row index.
Keep storage reads and decoding out of fill/action callbacks. Start/end/reverse
remain machine-page controls. The initial entry point below uses an explicit
track choice; a selected-track shortcut remains future work.

**Reusable listing requirement (2026-10-30):** expose file and directory browsing
through a shared SDK service usable by both the sampler and a later file manager.
The sampler owns sample-format filtering, preview and per-track binding. The
listing service owns provider/root selection, directory traversal, bounded page
requests, full names/paths, entry type and size, provider identity metadata,
completion/error states and stale-result rejection. Keep display truncation in
the UI. Do not promise that the DT2 hash and DN2 SHA have identical semantics or
that either listing pins file contents. Revalidate a selected identity when opening
it for loading. File mutation operations are a later extension to this service.

**Latest checkpoint (2026-10-30): public listing passes on both devices.**
The common `<dk/files.h>` contract is implemented; runtime 0.5.4 appends API 1.4
calls while retaining the existing pinned-bank API. All 94 SDK tests, no_std,
the owner pytest, generation/layer checks, CF objects and scoped TS checks pass.
Native runs: DN2 `out/runs/files-api/dn2/7b5255c0-1a6b-478e-9b46-063270a29efa`
and DT2 `out/runs/files-api/dt2/c8967bb4-e7a6-4856-bc00-8b5c628a9a04`.
See [finding 93](../findings/93-sdk-file-catalogue.md) for evidence and limits.
Both runs pass independent artifact review. Direct host checks pass 195 tests
(16 skipped); routed checks retain baseline stale references and the linked
pnpm launcher failure. No merge gate, hardware run or performance claim is made.

**Audit checkpoint (2026-10-30): owned backing and cache safety remain open.**
[D] Finding 106 now establishes a positive 512 KiB initialization extent at
`[0x804ad790,0x8052d790)` (sixteen 32 KiB per-track clears), not universal
runtime bounds or free space. Adjacent helpers/consumers remain open.
[D]/[C] Private 0.2 LP0 RX is `dsp.l2` module BSS, not the L1 allocator;
its historical 16-byte alignment leaves neighboring storage inside the rounded
64-byte cache lines. Private fixture 0.2.1 now isolates RX on 64-byte boundaries
inside a 4,224-byte reservation, owning the full 4,160-byte rounded footprint.
This supplies no coherence; native proof needs a new fixture and exact image
hash. Startup ranges make RX potentially cached under the audited register
constants, whose preservation through earlier calls remains unproved.
The stock maintenance helper needs range-0 ownership and an ABI/context audit;
writeback plus invalidate is not a safe generic post-DMA invalidate operation.
See [finding 95](../findings/95-dn2-cache-and-lp0-staging.md) for exact extents,
conditional configuration and public-manual citations. The emulator has no dirty
cache model and cannot prove physical coherence.

[D] On DT2 1.17 the stock sample path is DMA into non-cacheable staging, then a
SHARC CPU copy into cached DDR. Writer and reader are the same core, so the data
cache cannot make PCM stale and no maintenance is needed. An owned buffer filled
the same way needs no device test. Only direct DMA into cached memory needs
maintenance and one device test. See
[finding 108](../findings/108-dt2-sample-cache-visibility.md).

**[D] STOP callback ownership narrowed:** independent original-byte audit now
supports private single-descriptor STOP RX ownership from successful IRQ DONE
callback entry until explicit rearm, under private port ownership with no other
submitter. STOP disables LP/DMA and resets DMA address/count/config before the
callback; the inactive descriptor stays queued until replaced by submit. The
adapter copies before rearm. AUTO FLOW1/chained paths do not share that guarantee.
Dispatch is synchronous SEC IRQ context, not a deferred task, and IRQ masking
must not be assumed; exact nested-interrupt constraints remain open. Hardware
completion ordering does not establish cache visibility. Finding 95 records
addresses and public-manual limits. Production cache/pool/close-drain work remains.
Parent built/redecoded the 528-instruction tracked 0.2.1 DSP object and passed
`build --check`; CF is unchanged. RX ownership assertions now use order-independent
interval checks, with unit reservation bounds derived from assembled section size.
No new native fixture, run or hardware proof is claimed.

**[C] Encoding correction:** `0x1c0807` / `0x1c081b` currently execute Type 7a
conditional MODIFY under NOT SV. The earlier conditional-W2B conclusion is
plausible but **not proved**: public Type 7a/7d figures/tables conflict, and the
current decoder's selection rationale is unsupported. The earlier claim that
already-byte pointers remain unchanged is also retracted; public Selache lacks
conversion coverage and supplies no independent MODIFY proof. Helper coverage
remains conditional on address normalization. This is a suspected model
gap, not a confirmed fix; a universal shift is unsafe because conversion depends
on the memory map. Hold core changes/regeneration pending independent encoding
evidence; do not request approval based on false certainty about W2B.
No hardware or core regeneration is authorized. Continue read-only completion
and ownership investigation; production pool reservation, cache-safe RX and
CF/DSP close/drain ACK remain before binding, with the picker after binding.
The 0.2.1 routed check retains the known ten stale references and linked pnpm
launcher failures. Direct scoped lint, format and types pass; no new native run
or merge gate is claimed. The module-owned DSP object was rebuilt with the
existing patchkit; no SHARC emulator core was regenerated.

- [x] Prove private CF exact-tuple close/ACK gating against the assembled two-track
  reader, including paused-reader drain and unaffected-track PCM. Logical complete
  message snapshots only; 22 focused cases pass. No physical transport/cache/backing
  ownership claim.

The private logical close/ACK proof now connects compiled CF gating to the
assembled two-track reader using complete immutable numeric messages. It is
independent of the cache encoding question and grants no production backing
ownership. Matching DRAINED is required before reuse; malformed replies and
timeout quarantine the operation. Physical transport and concurrent snapshot
publication remain separate work; see finding 94 and the fixture contract.

A focused public-source search (five targeted queries) left the encoding issue
unresolved: official ADI PRM Rev 1.5 and Rev 1.4 repeat the figure ambiguity; no
official erratum or encoded conditional W2B example was found. Do not repeat a
broad search or declare a confirmed core bug from that evidence.

**[D]/[C] L1 staging investigated:** the DN2 allocator takes byte counts and
returns private L1 byte pointers, with a plain-arithmetic DMA/completer alias.
Its OOM path disables IRQs and enters a fatal loop, rather than returning NULL;
that allocator-based staging route is rejected **as proposed**. Roughly 4.3 KiB
of L1 RX staging would not cap the large DDR PCM bank, but no available L1
capacity was measured or allocated. Existing SDK allocators are CF-side; no
owned SHARC L1 code region or nonfatal allocator is exposed. Moving RX alone
also leaves stock submit's existing maintenance-helper dependency. See
[finding 95](../findings/95-dn2-cache-and-lp0-staging.md#l1-staging-option-and-allocator-failure).
Retain aligned L2 private fixture 0.2.1; no hardware/core changes or new API are
selected. Next work remains checked production reservation/cache maintenance
and real CF/DSP transport integration. The 22-case logical close/ACK proof is
complete and independent of cache encoding and production backing ownership.

- [x] Implement private LP0 0.3 CLOSE and exact CF ACK with separate reply
  snapshots, sticky ISR failures and bounded drain polling. All 73 owner cases,
  scoped static/generation/current-object checks pass; CF/DSP objects redecoded
  (450/727 instructions). Diagnostic controls are not connected to audio readers.
- [x] Complete independent code review and prepare the exact 0.3 boot fixture;
  cold idle and all four pre-exchange descriptor counts match. See finding 94.
- [x] Run the explicit `DIGI_LP0_CLOSE=1` native held-stock-audio/CLOSE case
  with the exact 0.3 boot fixture: one pass, three skips, 60.350-second selected
  test, exit 0. Full 235-page/960,000-byte transfer, exact diagnostic drain without
  resend, bank/canaries and finite/non-silent stock-audio windows pass. This uses
  count injection, not an actual paused native audio reader or other-track PCM
  proof; physical IRQ/cache and production ownership remain open. See finding 94.

**Resolved launch blocker:** the initial 600-second exclusive lock wait expired
behind another session's shared `verify runtime-fixture` process (observed PID
56574), without starting a native daemon or producing a test verdict. The later
resumed run passed at `out/runs/lp0-proof/27dbeea7-d1f2-415a-b98a-0a75c25fc3d1`;
log: `out/runs/lp0-close-fixture/native-close.log`. Implementation/review checkpoint
`b185fa1d` is pushed. The fixture audit passes, including independent current-object
relinking and all 117 symbol addresses. Independent native artifact audit also
passes payload/canaries, ordered journal, audio metrics and final status; the
intermediate CLOSE snapshot retention limit is recorded in finding 94.

**Exact native resume inputs:** ignored artifact
`out/runs/lp0-close-fixture/native-command.json` records cwd, command, environment,
480-second external deadline and exclusive lock requirement. Use the existing
prebuilt daemon/patchkit selected there; do not rebuild a core or resize live DMA.
Select only `sample-lp0-transfer.test.ts` / `closes diagnostic reader control`
with `DIGI_LP0_CLOSE=1`, the private ready directory and its `provenance.json`.
The fixture's patched image is `6ff3b5ec9d9ed80f1108746922a213a75c9827a867fb257c1690c419fa56a2c8`.
These inputs are retained for reproducibility. A new module/object edit requires
a newly matched fixture; the passing run does not authorize fixture reuse after edits.

**Next integration seams (read-only scout):** both machine adapters already
provide the latched source track in `dk_voice_in.track`. DT2 routes paired voices
to track state; DN2 keeps per-slot voice state with a separately latched source
track, so a DN2 voice slot must not become a binding index. The single-bank
`modules/core/adapters/dsp-ddr-proof/src/dsp/playback.inc` already pins each
note/render callback around backing access, but uses global bank controls. Its
legacy payload transport replaces stock audio and must not become the new
per-track route. Use its callback ABI as a reference and the isolated two-track
fixture's cached owner/epoch checks; keep LP0 transport integration separate.
The bounded 0.4 adapter below adds private per-track readers; production pool reservation remains open.

## Next bounded integration: real per-track machine readers

- [x] Integrate actual machine `note_on`/`render` callbacks with the LP0 CLOSE
  controls, using existing SDK PCM16/cursor and bank-lifetime primitives.
- [x] Keep A unready until all 960,000 bytes, guards and terminal rearm succeed.
  An earlier CLOSE must prevent later publication of the same epoch.
- [x] Give B a distinct module-owned 16 KiB PCM16 bank (8,192 frames), populated
  before publication. A diagnostic rate of 1/64 yields about 10.9 seconds without
  another unreserved DDR range. This rate is a test choice, not a product limit.
- [x] Route by source track, cache track plus epoch, acquire/release per callback,
  and reject unready/stale/closed voices before any sample read. Preserve the
  existing two-record wire tuple and single-use generation; no public binding ABI.
- [x] Register an explicit DN2-only private diagnostic machine at unused type 12,
  outside default module sets; preserve retired and dynamic type reservations.
- [x] Prove both real native voice paths start, CLOSE A through CF/LP0, retain
  explicit pending/final snapshots as applicable, observe A reads stop while B
  progresses and produces its known waveform. No injected reader count in this
  new case. Retain the separate interrupted-reader model and earlier transport
  cases; native completion alone does not prove an IRQ landed inside a read.
- [x] Audit new firmware/object/boot-fixture identities and native artifacts;
  update this plan.
- [x] Commit/push the verified real-reader checkpoint on the sampler discovery branch.

This step remains an emulator diagnostic. A's fixed test bank is not production
owned backing. It adds real reader integration while production pool reservation
and hardware cache safety are investigated separately. Existing DT2 loader/reset
queries establish slot-owned allocation and concrete project invalidation, not an
independent adapter allocation. DN2 runtime ring inspection now establishes masked recurrence but leaves the first
read conditional on producer range/rounding and SIMD state; see finding 106.
Configured DDR size is not evidence of a free sample pool. The DT2 follow-up
records project-reset invalidation in finding 81 and rejects a byte-fill helper
as an allocator candidate.

**Resume next:** connect the proved reader lifetime to owned backing and a
device-side close/drain acknowledgement before exporting per-track bindings.
The initial ordering and reader strategy are recorded in
[the binding design](../design/sample-binding.md); public ABI and production
backing remain unimplemented.
The private two-track consumer now passes all 15 focused cases: A can close,
drain and replace while B retains exact PCM; old A voices and mismatched cached
track identities cannot read replacement backing. Separate simulated stacks and
saved-frame checks correct an initial harness flaw. See
[finding 94](../findings/94-sample-binding-lifetime.md). This is a scheduler-model
proof, not physical interrupt dispatch, DMA/cache or a native device scenario.
The DN2 output-pointer table initializer is now resolved and independently
checked against original bytes: sixteen 128-byte output buffers at
`0x804acf90..0x804ad790`. This narrows provenance; it does not establish a free
pool. The apparent later writer `0x1c9118 → 0x254a84` is now corrected on its
audited first-pass path: byte-width addressing writes `0x254ab4`, outside the
table. Other callees/loop paths and consumer bounds still need tracing; see
finding 106. Do not repeat the initializer or this bounded width correction.
The old sample API still owns one nonreplaceable 8,192-frame bank per module;
it cannot implement this product by changing a path alone. Existing
`sdk/dsp/sample_bank.inc` supplies admission, epoch and reader-drain primitives.
Integrate those around every possible PCM read, and establish a DSP-visible
close/drain acknowledgement before reusing backing. Inactive voices need not
be forced to render if future admission reliably rejects their stale epoch.
The private 0.2 transport already proved device-side CF completion acknowledgements
with stock audio; that is separate from retiring an old audio reader. Production
pool reservation, cache safety and bounded transfer/recovery remain open.
Use the established source-track contract (0..15), stop only the selected track,
and publish the new generation only after source validation and DSP completion.
Keep the picker independent of provider internals and do not report READY early.

The initial backend was `modules/core/runtime/sdk/src/store/catalog.rs`: verified
open once, then one bounded table read per page, with header checks before and
after. It enumerates raw entries without sample filtering or prefix-total scans.
The runtime now uses incremental `CatalogOpen`, bounded directory projection
and same-generation rewind to form pages outside UI fill/action callbacks.
The byte audit now identifies a caller-owned 16-byte state and `0x114`-byte
entry; the initial C++ name-ownership inference was incorrect. Its locking and
sequential scan still require task-context scheduling. Details, validation and
remaining limits: [finding 93](../findings/93-sdk-file-catalogue.md).

**Earlier checkpoint:** DN2 backend is committed/pushed as `81d235b7`. The DT2
private UI-task diagnostic now passes its live test and independent artifact
review: root and child listings match stock ReadDir, dot entries are observed,
physical sizes differ by the documented 16-byte trailer, invalid paths and EOF
behave correctly, and guards remain intact. Run:
`out/runs/file-catalog/808e86fa-814f-4b46-92a0-660fc9c20162`.
This private proof preceded the public SDK acceptance above; a picker remains pending.

The listing implementation preserves full paths and provider metadata and
rejects results from superseded requests. DN2 projects bounded directory pages
over its flat table, with implied folders and deduplication; empty filtered
chunks are not EOF. DT2 wraps the proved iterator in task context and exposes
typed stored metadata. Concurrent source mutation still requires revalidation
at load. Keep source validation and sample ownership out of the listing API.
Current service decisions and ABI are in
[the reusable listing design](../design/sdk-files.md). UI callbacks lack current
module attribution, so clients are acquired during initialization; generations
reject late results. Notifications must not require owning the sole module slot 0.

**Initial integration route:** use the existing `DK_MENU` entry with a track list,
then a reusable `DK_UI_LIST` browser for that explicitly chosen track. This makes
the target visible and does not depend on an undeclared current-track getter.
A machine-page shortcut can later open the same browser with a supplied track.
Reuse the menu object; retain target, folder, page and row identities in persistent
context. YES on a file queues selection; NO returns without changing the binding.
Show the selected binding and queued/loading/ready/error status separately from
the currently highlighted catalogue row.

Catalogue implementation runs on the device. Existing TypeScript
`RpcSamples`/`StoreSamples` demonstrate host-side listing semantics; they are not
callable CF backends. DN2 uses the runtime store scanner and header fingerprint;
DT2 uses the audited firmware directory adapter. Neither existing
host listing exposes a catalogue generation snapshot. Preserve DN2 path/size/SHA
and DT2 path/hash/size where available, and revalidate at selection/load rather
than treating a row number as a durable reference.

Validation will use the existing UI host fake for track capture, navigation,
cancel, paging and stale-result rejection, then device-specific catalogue tests
and bounded DN2/DT2 emulator picker flows. Do not build a picker that reports a
successful binding before the runtime can actually own and publish that sample.

**Latest checkpoint: private device-side completion proof passes (2026-10-29).**
Diagnostic 0.2 changes both boot-time wire counts from `0xabc` to `0xacc`, keeps
the stock `0xabc` copy, and carries a coherent completion snapshot in the new
16 bytes. CF sends each retained request once and waits for the exact DSP ACK.
First-page and full-bank held-stock-audio proofs pass: 235 CF acknowledgements,
960,000 exact bytes, intact guards, terminal DMA inactive, finite non-silent PCM
before/during/after, and no silent window in 415 measured transfer segments.

- [x] Implement the private 0.2 reply extension.
- [x] Complete 31 focused tests, scoped static checks and independent review.
- [x] Prepare a private cold-idle from pre-init state; verify all four counts
  `0x566` before any CF exchange, without live DMA edits or core regeneration.
- [x] Run the first-page and full-bank audio proofs and independently audit artifacts.
- [x] Run direct host checks: 195 passed, 16 skipped. Routed checks retain the
  ten baseline stale references and linked-package launcher failures; no merge gate claimed.
- [x] Resolve first-release UX: on-device picker, confirmed by Em on 2026-10-30.
- [ ] Design the production binding/loading API and owned backing for that choice.

Evidence and fixture-selection correction: [finding 88](../findings/88-dn2-reply-ownership.md).
The first 0.2 run selected the old cold-idle through a canonicalized image symlink;
a physical private image copy and explicit preflight guard fix this. No firmware
or emulator-core correction was needed. Production pool ownership, cache behavior,
bounded production transport and generation replacement remain open; no hardware
operation or public completion contract is claimed.

**Earlier payload checkpoint: DN2 native bulk transfer with stock audio (0.1).**
The private LP0 adapter uses the existing CF sender and SHARC driver. The
first-page proof passes, then the separately selected full-bank proof passes:
235 packets, 960,000 exact bytes, final 1,536-byte clamp, intact 4 KiB guards,
terminal DMA inactive, and subsequent ordinary DSP frames. Checkpoint `31d96245`
is pushed. A subsequent held-stock-note case also passes: finite non-silent PCM
before, during and after the full transfer, with no silent 100 ms window in the
measured transfer interval. This is emulator evidence, not production pool
ownership or hardware safety.
See [finding 92](../findings/92-dn2-native-bulk-seam.md) for artifacts and limits.

- [x] Implement private task-context setup, guarded callback and descriptor rearm.
- [x] Pass 21 focused tests, scoped static checks and independent review.
- [x] Run byte-exact first-page and full-bank native proofs.
- [x] Prove finite non-silent stock DN2 playback before/during/after transfer;
  record per-window metrics without claiming dropout-free hardware audio.
- [ ] Bound stock fixed DDR object access before choosing production reservation.
- [x] Prove a private device-side completion channel preserving stock reply data;
  production ownership and integration remain separate (finding 88).
- [ ] Integrate per-track loading/publication and prove both devices.

Ownership audit: fixed FX object rooted at `0x8005912c` has bounded ring indices
and a layout ending at `0x80459388`; workspace bounds remain conditional on the
live frame count. Continue with the next fixed roots and output-pointer provenance.
Existing small allocation entry is an L1 heap;
no general reservable DDR arena has been established. Keep the experimental bank
private and unpublished to voices until ownership is resolved.
The following smaller ring object is also locally bounded; a lookup index is
bounded by a signed-halfword load, correcting the initial unbounded-word reading
(finding 106). The explicit reply extension is now proved in the private diagnostic,
with boot-time counts and unchanged stock-copy bounds.
No stock header/trailer bytes are approved for reuse.


**Confirmed (2026-10-29): independent per-track sampling.**
Changing a track's sample stops that track's old notes; unrelated tracks keep
playing. Em confirmed this interpretation and asked us to continue until a real
decision is needed. Parser checkpoint `db3514f7`, audit checkpoint `bc7ad3a6` and bounded source
checkpoint `3b402543` are pushed. Source-track checkpoint is `03b6310e`. The existing spare-frame channel has a theoretical 72,000-byte/second
ceiling on DN2; 960,000 bytes require at least 13.33 seconds before overhead.
This is not measured loading time. The diagnostic's global mute is unsuitable
for runtime integration.

Current parallel work (source cursor reviewed; 59 SDK tests, no_std and owner pytest pass):
- [x] Resolve loading policy and record independent per-track selection.
- [x] Map SDK input/version checks: append-only input needs a machine minor gate;
  DN2 physical voice slots must not be treated as track IDs.
- [x] Trace the current DN2 frame's source-track producer and same-handler assignment ordering ([finding 90](../findings/90-dn2-track-owner-and-worker.md)); dynamic reassignment proof remains open.
- [x] Trace DN2 background-worker callbacks against current image bytes ([finding 90](../findings/90-dn2-track-owner-and-worker.md)); no sample allocator follows from generic worker dispatch.
- [x] Add bounded DN2 store reads with generation checks and mandatory digest completion ([finding 87](../findings/87-pcm16-streaming.md)).
- [x] Audit DN2 DDR configuration: 512 MiB configured geometry, stock scratch beyond loaded extent ([finding 106](../findings/106-dn2-ddr-configuration.md)).
- [ ] Establish explicit DN2 pool reservation from runtime pointer origins/bounds; configuration is not ownership.
- [x] Implement machine contract 1.1 source-track input (runtime 0.5.3), with DN2
  delayed-note latching and invalid-owner isolation; 17 focused + 8 adapter tests,
  generation/object/header checks and independent review pass (finding 90).
- [ ] Integrate owned backing, per-track generation binding and live transport.
- [x] Audit DN2 generic 4 KiB FlexBus sender and generic LP0 driver ([finding 92](../findings/92-dn2-native-bulk-seam.md)); empty function matches did not mean absence.
- [x] Enable/test DN2 1.12 emulator LP0 delivery: six peer tests and one profile
  test pass, reviewed; runtime-only native build, no generated-core changes.
- [x] Establish generic driver setup/callback/submit ABI and preflight obligations (finding 92).
- [ ] Implement/prove private receive setup/completion/rearm and byte-exact bank
  transfer during normal audio frames. Refuse owned/active port; initialize from
  task context. Existing driver open and submit mutate state before failure.
  Payoff: bypass the 72 kB/s SPI ceiling and occupied reply rows for payload.
  Confidence: moderate until receive setup and concurrent audio run; reuse the
  experimental bank, with no new production reservation or hardware operations.

Checkpoint checks: changed-document pathcheck, diff whitespace and direct generated
JSON formatting pass. Routed `test:changed` passes scoped Python/layer checks but
retains ten baseline stale references and the pnpm linked-node_modules launcher
failure for host/format/types. Direct host tests pass 195 with 16 skipped across
the initial run and the six socket tests rerun with local-socket permission; the
initial six failures were EPERM. Logs: `out/track-changed.log`, `out/track-host.log`,
`out/track-host-sockets.log`. No full merge gate or hardware run is claimed.

Completed static audits:
- [x] [Finding 88](../findings/88-dn2-reply-ownership.md): current DN2 1.12 ACK/diagnostic row bytes reach outgoing records. Header +0x08 is only a candidate and has a stock DMA-buffer accessor of unresolved reachability. No safe eight-byte reply slot is established.
- [x] [Finding 89](../findings/89-dt2-waveform-workspace.md): the 32 MiB pool above 0x80a00008 is a recurring min/max summary workspace with a forward bump allocator. It cannot be borrowed as free sample memory.
- [x] Source consistency: DT2 read calls serialize individually; no cross-read immutable lease or digest is established. Concurrent same-size Transfer replacement remains unsupported by the current source path.
- [ ] Choose and validate transport and backing integration that preserve unrelated tracks. No hardware probe/upload has been authorized or performed.

**Runtime integration in progress (2026-10-29):** Em asks us to continue
through routine implementation/checkpoints and pause only for a real decision,
with occasional progress updates. Preserve unrelated audio during sample loading
as confirmed by Em; do not inherit the diagnostic's global mute.

The completed independent implementation is a bounded-memory native PCM16 stream
validator/packer in the shared runtime SDK. It accepts fragmented input and
bounded output, validates the canonical mono 48 kHz header/trailer, and emits
little-endian packed words with odd final-frame padding. A late validation error
invalidates all staged output; nothing becomes READY until complete validation
and source-consistency checks succeed. The existing pinned float32 ABI stays
unchanged. This removes the need to stage/decode the entire ten-second source in
the 512 KiB CF arena; it does not itself establish an immutable source lease.

- [x] Map transport geometry and existing source snapshot constraints.
- [x] Implement and test bounded streaming validation/packing with backpressure.
Streaming checkpoint: 53 shared runtime SDK tests, the named owner pytest and
host `no_std` compilation pass. The new parser emits only complete packed words,
handles odd final frames and output backpressure, and fails sticky on bad/truncated
or trailing input. It does not change the current decoder/runtime/public ABI.
Evidence and integration gates: [finding 87](../findings/87-pcm16-streaming.md).

- [ ] Establish owned DSP backing and source-consistency strategy for integration.
- [ ] Connect serialized runtime selection/retirement/loading/publication.
- [ ] Add module controls and user-facing selection, then prove on both devices.

**Current checkpoint (2026-10-29): shared lifecycle primitives extracted.**
Em approved implementation and parallel independent agent work. The SDK now
provides `sdk/dsp/sample_bank.inc`: caller-bound mask/restore, admission checks,
fresh/cached generation handling, acquire/release, fail-closed bookkeeping,
close/drain and publication. Both diagnostic producer and consumer use it.
Wire validation, exact successor policy, state storage and transport remain
caller-owned. The six-word numerical cursor and public pinned float32 bank ABI
are unchanged; no general allocator or public selection API was added.

Validation: 99 focused assembled tests pass (84 existing + 15 independently
bound SDK cases). Independent review finds no issues. The DSP object still has
912 instructions; only its source fingerprint changed. Code/data sections,
relocations and symbols are identical to `e5c274d6`. Parent rebuilt complete
patched DT2 1.17 and DN2 1.12 images with the existing CLI and verified exact
identity with the previously passing images. Evidence:
`out/runs/sample-bank-extraction/image-identity.json` and
[finding 86](../findings/86-sample-bank-sdk.md). Direct host checks pass 195 with 16 skipped; scoped path checks pass. Routed
checks retain baseline reference and pnpm launcher failures (finding 86). No
emulator/core regeneration or coupled rerun is needed for unchanged firmware bytes. Instruction identity
preserves the schedule; it is not a hardware performance measurement.

- [x] Map consumer/producer lifecycle and existing runtime upload ownership.
- [x] Choose parameterized, instruction-identical primitives and caller-owned policy.
- [x] Implement SDK include and refactor both diagnostic paths.
- [x] Test alternate state/register/label bindings and lifecycle failure behavior.
- [x] Independently review contracts and compare complete patched image identity.
- [x] Record the extraction evidence and runtime integration boundary.
- [ ] Next: integrate a runtime-owned replaceable packed-PCM16 bank and sample selection.

Runtime integration must reuse existing source snapshots and `sample_job::Job`
source-retention behavior instead of creating a parallel loader. Its current
pinned mono-float32 ABI has neither generations nor replacement and cannot be
silently changed. The larger backing still needs an explicit owner/reservation,
a packed-PCM16 source/transfer path, and serialized select/retire/load/open state.
The SDK now supplies the DSP building blocks; production ownership, device
transport and UI integration remain separate tasks. Preserve stop-old-notes and
late-note/current-generation policy; uncertain transfer completion never permits
reuse. Hardware reservation/cache/coexistence are still open.

**Current checkpoint (2026-10-29): DT2 lifecycle port is implemented and all six
cases per device pass across the initial and corrected runs.**
DT2 uses normal frame length `0x802`, DN2 `0xa80`; both retain the diagnostic
`0xa80` send and `0xabc` reply. The DSP object and SDK reader are unchanged.
Independent original-image audits establish the transport and a boot-stream gap
for the bounded emulator bank; they do not establish runtime/hardware ownership.
DT2 diagnostic type 11 preserves retired type 10 (historical XSLICe).

Validation: 84 focused assembled CF/DSP tests; 195 host passes/16 skips;
scoped lint/types/object/path checks. Initial suite: nine passes and three DT2
playback-setup failures due to treating append insertion value 255 as a menu row.
The corrected test verifies the live menu membership and selects the observed row;
it also reads the correct per-device dispatched-type diagnostic. All six affected
playback/lifecycle cases pass on both devices in
`out/runs/ddr-proof/dt2-port-playback-corrected.log` (443.42-second wall time,
not a performance measurement). The six rejection cases passed in
`out/runs/ddr-proof/dt2-port-suite.log` and were not unnecessarily repeated.

DT2 full playback artifact: `out/runs/ddr-proof/dt2/a3273fdd-d212-4cb8-98ce-025ed8d6c427`.
Terminal retirement: `out/runs/ddr-proof/dt2/4f054e94-6ff7-4483-91b3-22164872ea6f`.
A → B sealed skip → C: `out/runs/ddr-proof/dt2/efe92f48-b20f-4b3c-9526-fa462c245b34`.
[Finding 85](../findings/85-dt2-sample-lifecycle.md) records raw-image provenance,
independent audits, hashes/audio and limits. The existing prebuilt DT2 emulator
reports generated DSP core `dt2-1.16` while executing DT2 1.17 firmware; no core
regeneration was performed. Ten baseline stale references and the pnpm launcher
failure remain in routed changed checks; no clean integration gate is claimed.

- [x] Independently audit DT2 transport and candidate bank against original bytes.
- [x] Port private diagnostic and per-device tests; preserve retired type numbers.
- [x] Review and correct live-menu lookup and device-specific dispatch observation.
- [x] Complete all six coupled cases per device; audit retained DT2 playback/lifecycle evidence.
- [x] Record validation and independent audits in this plan and finding 85.
- [x] Extract shared DSP lifecycle primitives and document runtime ownership boundaries (finding 86).

The next extraction should separate numerical reader/cursor operations from bank
publication, reader admission/retirement and device transport. Keep the proven
stop-old-notes policy and exact successor generations. Public sample selection,
Transfer integration and hardware reservation/cache/coexistence remain separate
work; do not expose this hardcoded diagnostic bank as a general allocator.

**Prior DN2 checkpoint:**
**Current checkpoint (2026-10-29): private generation reopening is implemented and all six coupled cases pass.**
The user selected: stop currently playing notes on selection; queued notes starting
after new READY play the new bank. Exact successor tokens, sealed COMMIT, explicit
OPEN, operation-specific ACKs and ACK-derived held-note loading grants are implemented.
The private host writer owns whole transactions and stops permanently after uncertain
completion, even when callers catch the error. There is no cancellation: seal B,
retire B, then load C.

Validation: 68 focused assembled diagnostic/CF tests; 195 host passes/16 skips
(`out/runs/ddr-proof/reopen-host-checks.log`); six coupled passes
(`out/runs/ddr-proof/generation-reopen-suite.log`, 202.04-second wall time, not a
performance measurement). A → B sealed skip → C artifact:
`out/runs/ddr-proof/5103437f-373f-4f40-b9f9-c2ef10a73698`. C's full readback matches
its source, guards remain intact, a fresh note uses C, and stale-B RETIRE times out
without changing C or preventing resumed normal frames. An independent audit confirms
all 72,544 C frames finite and window RMS 0.0094734175. The tightened sixth-case
finite/RMS assertions also pass in `out/runs/ddr-proof/generation-reopen-audio-check.log`;
artifact `out/runs/ddr-proof/9a8eff19-98bf-4d2d-b643-8461f07fd6a2` retains 72,448
finite frames and the same measured window RMS. Metrics save before audio verdicts.
Objects re-decode 370 CF / 912 DSP instructions.

The ten known reference failures and pnpm launcher failure remain in changed routing
(`out/runs/ddr-proof/reopen-changed-checks.log`); no clean integration gate is claimed.
[Finding 84](../findings/84-sample-generation-reopening.md) retains exact hashes,
audio evidence and scope. Next: port the owned-bank lifecycle proof to DT2, then
extract the reusable SDK/runtime contract for both devices. No public API or new
hardware pool ownership is approved; hardware cache/coexistence remain separate work.
The queued-note policy is settled.

**Prior independent guard checkpoint (2026-10-29): the independent cached-voice generation guard is complete.**
The private seventh state word preserves the six SDK cursor words; state/clear size is
28 bytes. Forty-two focused assembled diagnostic/CF checks and all five existing coupled
cases pass. Host checks pass 193 with 16 skipped. Coupled log:
`out/runs/ddr-proof/voice-generation-suite.log` (119.59-second wall time, not a performance
measurement); normal artifact `out/runs/ddr-proof/55f9f2f3-ff27-47d5-aa3b-1d80f4054be6`,
terminal artifact `out/runs/ddr-proof/d1974ff3-48fa-46f1-8bec-6feb6b6f9314`.
[Finding 84](../findings/84-sample-generation-reopening.md) records the guard and proposed
next protocol. Those coupled results predate the explicit-OPEN wire changes. The selected policy stops
currently playing notes on selection; queued notes that start after new READY play
the new bank. The ten known path-reference
failures and pnpm launcher failure remain in routed changed checks; no clean gate is claimed.

**Prior terminal-retirement checkpoint:**
All five coupled cases pass: full 960,000-byte transport and ten-second playback,
three fresh-boot failure cases, and terminal retirement during progressing playback.
Log: `out/runs/ddr-proof/terminal-retirement-suite.log`; its 121.38-second wall time is
not a performance measurement. Normal playback artifacts:
`out/runs/ddr-proof/6f65ad4c-3cf6-4553-8500-15eadd79bc51`. Terminal artifacts:
`out/runs/ddr-proof/ff8f8057-0a9f-4030-95ff-83143b7ac2a5`.

The terminal request observed cursor 49664/DONE=0. Exact RETIRED then reported
READY/open=0, closed=1, admitted=0, fault=0 and 1,572 source-kernel calls, below natural
completion at 15,000. Only after that ACK did the test poison all 960,000 backing bytes.
Both full canary bands retained the expected pattern. Source-kernel calls remained 1,572
while the original trigger stayed held, after an actual fresh note-on, and after duplicate
RETIRE. Normal frames progressed after poison; callbacks returned DONE and no fault.
[Finding 83](../findings/83-sample-retirement-design.md) retains hashes and observation limits.

The private terminal contract opens admission only at the first complete COMMIT.
RETIRE (op 4, seq 377, exact bank token, zero offset/count) permanently closes admission
and READY. Status 6 means PENDING while an admitted callback remains; only exact status
5 means RETIRED. CF retains one immutable staged request and retries boundedly; timeout
never permits backing reuse. Admission, final-read release and closure use short
IRPTEN-masked sections, two conservative NOPs after clear/restore and exact incoming
MODE1 restoration. The numerical reader runs with incoming interrupt state restored.
Assumptions remain one core, serialized adapter/hook paths and maskable accesses;
no universal firmware/NMI exclusion is claimed. Corrupt bookkeeping fails closed.

Raw first/latest MODE1, ASTATX, I6/I7/I13 and active markers are bounded, asynchronous
observations. MODE1 was `0x39011cf8`; ASTATX is not a definitive task/IRQ identity.
Tail entry/exit reads (35355/35358) straddle execution and are not a coherent counter
snapshot. Zero observed tail overlap does not establish global exclusion.

Validation: 36 focused assembled diagnostic/CF checks pass, including preserved-reader
interleavings before first read, mid-reader and after kernel return. These test the
protocol against shared DM, not physical IRQ dispatch. Rebuilt objects re-decode 264 CF /
813 DSP instructions. Host checks pass 193 with 16 skipped
(`out/runs/ddr-proof/terminal-host-checks.log`); direct lint/format, regression TypeScript
and scoped path/diff checks pass. `test:changed` retains ten known path-reference failures
and the pnpm launcher failure (`out/runs/ddr-proof/terminal-changed-checks.log`); this is
not a passed routed integration gate.

**Next:** port the owned-bank lifecycle proof to DT2, then extract the reusable
SDK/runtime contract for both devices after the focused tighter-audio rerun and final review. Stop currently
playing notes on selection; queued notes that start after new READY play the new bank. The terminal bank never
reopens. Product sample APIs, Transfer UI integration, external DT2 mutation interception,
DT2 ten-second backing and hardware ownership/cache/coexistence remain separate work.

**Completed independent guard (2026-10-29): cached-voice generation fencing.** The diagnostic now
reserves a private seventh state word (28-byte state/clear size), leaving the six-word
SDK cursor unchanged. A fresh accepted note latches the nonzero bank token inside the
masked admission section; render rejects a zero or mismatched cached token in that same
section before incrementing the admitted count. Denied callbacks invalidate the token.
Forty-two focused assembled diagnostic/CF checks pass, including six new cases for a
changed global token, fresh-note capture, denied notes and armed-like cached cursors.
The DSP object rebuilds/re-decodes 832 instructions and passes its freshness check.
This checkpoint does not implement reopening, queued-event fencing or serialized writers.
Queued-note policy is selected as above; the coordinator retains final integration
verification. [Finding 84](../findings/84-sample-generation-reopening.md)
records the selected policy, implemented exact-successor/sealed-COMMIT/explicit-OPEN
protocol and whole-transaction writer requirements. All five existing coupled cases
pass with the changed consumer; no actual bank reopening is claimed.

- Worktree: `/private/tmp/digikit-dsp-sampler`; branch: `refs/heads/sdk/dsp-sampler-discovery`.
  Keep Claude's original checkout on its cleanup branch untouched.
- Read this plan, then [finding 104](../findings/104-dsp-sdk-sampler-discovery.md). Check git status and the
  cleanup checkpoint before implementation; the module inventory is pinned to `219570ca`, not final cleanup.
- First approved implementation is now integrated: exact xorshift and BD/PD sine-polynomial SDK macros with
  consumer replacements; split cursor and bounded mono linear-reader kernel with numerical references/tests.
  Read the implementation checkpoint below for verification status. Sample transport, stereo ABI and hardware
  work remain separate decisions. Em has now approved the bounded sample transport/lifetime proof below.
- SDK checkpoint: `4cd29a3c`. The resumed loading/lifetime investigation is in
  [finding 105](../findings/105-sample-loading-lifetime.md). Read the next proof proposal below before changing runtime
  code. It preserves Transfer destinations and uses a module-owned, session-pinned DSP bank. The proof is approved;
  do not ask for its approval again. Source, packaging and runtime patches are integrated.
  Both DN2 1.12 and DT2 1.17 transferred-file acceptance now pass after the ColdFire byte-swap helper and explicit-label
  DK_CALL corrections: oversize rejection, clean restart, actual type 16 dispatch, unready silence/no fault,
  exact published bytes, READY, replacement refusal, audible playback and retriggering. Log:
  `out/runs/dsp-sdk-checks/sample-playback-fixed-dn2.log` and
  `out/runs/dsp-sdk-checks/sample-playback-dt2-retry.log` (each one pass, the other device filtered out).
  The bounded emulator proof is complete; production capacity, replacement and sample selection remain proposed decisions.
  Use git log for the final implementation checkpoint commit.
  No transport or core-semantics change was made for the call-frame fault. No hardware run is authorized.
  [Finding 79](../findings/79-sample-bank-proof.md) records the evidence and remaining limits.
- Capacity discovery is recorded in [finding 80](../findings/80-sampler-capacity-and-replacement.md): four snapshots
  establish baseline and proof-only module pools, not stock free RAM or general coexistence. The CF arena is
  512 KiB on both devices; shared DSP module areas are 512 KiB DT2 and 320 KiB DN2. Decoded CF source memory
  is freed after terminal transport; only the DSP bank stays pinned until restart.
  The historical prototype target was approximately ten-second one-shots; stop old notes on selection remains required. Ten seconds of 48 kHz
  mono PCM16 is 960,000 bytes (stereo twice), beyond either current reserved DSP module pool. The official DT2
  manual documents 400 MB project sample RAM; our module reservations are not hardware memory limits.
  [Finding 81](../findings/81-stock-sampler-dependencies.md) now records the stock-machine-led dependency map
  and corrects the earlier preview-loader lead. The executed OneShot slot/binding and active replacement
  follow-up is in finding 82; start/end/reverse and project reload remain open. Derive independently written SDK
  operations, rather than copy a complete machine.
  Proposed shared packed-PCM16 reader/cursor/region semantics use device-specific ready immutable views;
  runtime owns selection/loading/lifetime and stock downstream envelope/filter remains. The earlier mono-first proposal is historical; full stereo is now required.
  DN2 already plays samples through the working voice seam/source acquisition; investigate its missing larger
  backing/delivery against DT2's dependencies, not playback feasibility from scratch. No new backend is implemented.
  Finding 83's independent DN2 1.12 raw-byte audit withdraws finding 80's external-read leads at `0x1c8ef7`
  and `0x1c8f00`: the former is not an instruction boundary and the latter is a register move. Larger backing
  remains unresolved; next trace initialization, allocation/ownership and cross-port delivery. Missing exact
  address literals do not establish absence, and module reservations do not establish hardware memory limits.
- [Finding 82](../findings/82-stock-sampler-lifetime.md) records the executed slot/lifetime discovery and exact
  image provenance. Stock OneShot selection and ten-second acquisition are established. Both the identified
  patched-image trial and the passing unpatched DT2 1.17 critical reproduction retain a progressing active voice
  address while exact backing bytes change; cleared slot metadata is not retirement. Next design requires explicit consumer retirement
  before overwrite/release, stale-note generation guards, and coverage of external slot replacement/project reset.
  Borrowing the DT2 stock pool is the chosen direction ([finding 109](../findings/109-dt2-stock-pool-borrowing.md)); preserve the current immutable-bank proof until the adapter exists.
- [Finding 83](../findings/83-sample-retirement-design.md) records the retirement discovery,
  corrected AudioTask roots, explicit IRQ/admission assumptions and passing terminal DN2 result.
  Both CF builders submit the prior frame before queue traversal/reconstruction; strict UI-time
  generation binding remains unresolved. The actual AudioTask entries lie inside merged database
  function extents; earlier same-parent voice/tail shorthand is corrected in finding 83.
  The private terminal bank proof closes/drains before reuse but never opens a new generation.
  Current DN2 960,000-byte backing/delivery and ten-second playback are established only for the
  isolated emulator diagnostic. Earlier source-survey drafts are historical; external DT2 mutation,
  hardware ownership/cache/coexistence, strict stale-event fencing and serialized next-generation
  writers remain separate design/proof obligations.
- Use workers for scoped implementation, in separate writer worktrees if parallel; keep design and integration
  in the parent. The user confirmed the personal spawn-policy hooks were reviewed and trusted in this session.
  A new session must satisfy its own applicable hook/trust requirements before delegation.
- Read-only sample delivery/lifetime discovery can continue independently. Do not launch competing gates/builds.
- Shared symlinks are input databases/manuals and `.venv`; local `out/cache/` and run outputs are isolated.
  Use `UV_NO_SYNC=1`, worktree-local `UV_CACHE_DIR`, and local `MISE_STATE_DIR` as needed. Never rebuild shared DBs
  implicitly with `sharc.load()`; use read-only queries.
  The resumed source investigation also links the existing `out/ghidra/` dump directory read-only.
- Documentation and the first approved SDK implementation are now present. Validation and known baseline failures
  are recorded below; do not mistake the numerical sample reader for a complete sampling machine.
  Em authorized committing and pushing this branch; checkpoints have been pushed. Consult git log for current
  commit identity. No merge is claimed here.

## Objective and scope

Extend the device-independent module SDK with reusable DSP operations, migrate existing machines and effects,
and build a first one-shot sampler for Digitakt II OS 1.17 and Digitone II OS 1.12. Name: SHOT.
Initial musical scope: full-stereo sample selection, start/end, reverse, and normal note pitch
on both devices, with 16 tracks sharing a deduplicated project pool and no fixed
per-track partitions. Long-sample limits follow real capacity and supported formats;
stock DT2-like capacity is a target awaiting proof. Granular playback, slicing,
time stretch, wavetable synthesis and multisampling are future consumers, not first-release requirements.

The payoff is a reusable sample-access contract and tested kernels that make later machines cheaper to build.
Confidence is high in the discovery approach; DN2 playback is proven for the bounded bank, while larger backing
capacity, ownership and delivery remain open. If sample delivery requires a large unrelated runtime redesign, stop and present alternatives
before extending scope. Do not survey every stock machine before delivering a sampler decision.

## Agreed direction

- Device independence is required: content modules use SDK contracts, never firmware addresses or layouts.
- Keep mathematical contracts and portable reference models; implement an optimized SHARC backend today.
  A universal processor instruction abstraction is deferred.
- Move reusable DSP operations and platform access into the SDK. Module orchestration, state transitions and
  distinctive algorithms can remain module code; wrapping every assembly instruction is not the goal.
- Offer small step operations and scheduled block kernels. Add fused kernels when a measured consumer warrants
  them; sequential macro expansion must not force a performance regression.
- Use both public SHARC manuals and targeted firmware analysis. Manuals establish semantics; firmware reveals
  integration, ownership and scheduling. Describe interfaces and idioms without copying firmware code.
- Reuse SQLite helpers and NetworkX graphs. Extend tooling only for a concrete unanswered question.
- Prefer bounded resident playback for the first sampler if the memory and transport evidence supports it.
  Card storage capacity is not a playable-RAM budget. Streaming is a separate decision.
- Em confirmed on 2026-10-29 that DN2 has no stock sampling. Its sampling service is new digikit functionality;
  general SHARC kernel research still covers both devices equally. Stock class names do not imply DN2 playback.

## Workspace and coordination

- [x] Inspect the current checkout without modifying it; Claude owns `cleanup/self-contained-modules`.
- [x] Create branch `refs/heads/sdk/dsp-sampler-discovery` in `/private/tmp/digikit-dsp-sampler`.
  Baseline: `219570cafe73da2beb605cdebe2f04950968b960` (cleanup groundwork checkpoint, not final cleanup).
- [x] Keep generated outputs local to the new worktree; do not run the shared-output worktree setup script.
- [ ] Record the final cleanup checkpoint and reconcile the inventory before module migration.
- [ ] Before builds, identify shared artifacts, use unique run outputs, and coordinate the CPU/build budget.

No checkout, reset, stash, cleanup, or edits in Claude's tree. Read-only database/manual inputs may be reused;
do not mutate them. No dependency changes in a shared environment. No hardware action without Em's decision.
The parent owns decisions, plan updates, integration and verification; scouts read, workers implement only after
the implementation decision, reviewers audit. One writer per worktree; children do not delegate.

## Phase 1 — contract and module inventory

- [x] Read SDK, module and firmware-query guidance and the latest handover.
- [x] Confirm the distinction between transferred/stored samples and DSP playback access.
- [x] Locate existing SQLite/NetworkX graph helpers and DSP reference tests.
- [x] Inventory pluck, bd, analog, pd and freqshift: initial source inventory below; measurements and final cleanup reconciliation remain open.
- [x] Audit the machine/FX ABI: source-level gaps and device adapter behavior recorded in finding 104; firmware-owned buffer lifetime remains open below.
- [x] Record current checks, timing-model limits and memory constraints from authoritative code/configuration; measurements remain pending.

Acceptance: an evidence-backed gap matrix and migration order, with implemented behavior separated from proposals.

## Phase 2 — sample path on both target images

- [ ] Inventory available current-image databases, dumps and provenance; do not rebuild them implicitly.
- [x] Trace DT2 1.17 selection/load/slot registration through ColdFire to SHARC-readable PCM and voice access. Done: findings 81, 82, 109.
- [x] Record current DT2 project loader/release/reset, slot/setup/render dependencies and DN2 hash-comparison limits
  in finding 81, including independently audited rebase/store/tap bytes. Finding 82 now captures the current
  OneShot UI; broader CF/lifecycle audit and controlled parameter/boundary semantics remain open.
- [x] Capture current stock OneShot slot binding and the unique ten-second a → b active replacement transition
  on the original DT2 1.17 image; retain exact source bytes, active/cursor state and PCM (finding 82).
- [x] Persist retirement discovery and adapter/trigger review in finding 83; the private terminal DN2 diagnostic is now implemented and passes, without a public ABI.
- [x] Read both current CF builders' prior-frame submission order, DN2 timestamp queue/reuse helpers and
  both-device same-parent static voice/tail chains, submit-guard absolute references and DN2 staging/register
  semantics; record existing diagnostic capabilities and limits in finding 83.
- [x] Prove terminal DN2 owned-bank retirement under explicit one-core, serialized adapter/hook and maskable-access assumptions, with assembled interleavings and coupled held/retriggered coverage.
- [x] Prove private DN2 reopen/new-generation publication and serialized writers under the selected policy: queued notes starting after READY use the new bank; cached old voices cannot adopt it.
- [ ] Prove DT2 retirement separately and cover external replace/unload/project reset before borrowed-view exposure. Start/end/reverse captures remain open.
- [ ] Map DT2 dependencies against DN2 1.12's working voice seam and source acquisition; identify missing larger
  backing ownership/delivery without assuming DT2's pool or LP0, or restarting playback feasibility from scratch.
- [x] Establish transferred-file format/rate and the implemented bounded float32 delivery path; the proposed
  packed-PCM16 reader is separately implemented and tested in the fixed DN2 diagnostic.
- [ ] Establish placement, capacity, alignment, address representation, ownership, retention and eviction.
- [ ] Establish ready/failure/unavailable behavior and synchronization for sample replacement and active voices.
- [ ] Check retriggering, voice stealing, reverse boundary access and parameter changes during playback.
- [ ] Decide resident versus streaming feasibility, with explicit bounds and missing evidence.

Initial bounded investigation is complete with precise blockers; the remaining boxes require deeper traces or an
approved proof. Findings: [104](../findings/104-dsp-sdk-sampler-discovery.md). DT2 slot metadata and page-copy arithmetic
were re-read on 1.17; stock allocation/retention is not yet safe to reuse. DN2's existing link is suitable to investigate
for resident data, but its 72,000 B/s theoretical payload ceiling is below mono 48 kHz int16 streaming's 96,000 B/s.
This rules out that representation through that link, not every possible streaming design. No general DSP work
needs to wait for a new sample transport or a full stock-machine census.

Use `tools/coldfire/fw.py` atlas/catalog/dossier/batch first; use `tools/sharc_dev/sharc.py` CFG/callgraph/defuse
and `tools/coldfire/cf.py` for focused graphs. SQL is read-only and follows existing helpers. Graph absence is not
proof of absence: inspect indirect dispatch and data references. Resolve DT2 1.16 findings on 1.17 before use.
All new firmware claims stay [D]/[O] until an independent agent checks image bytes. Findings belong under
`docs/findings/` and are indexed in `docs/FINDINGS.md`; this plan tracks work and decisions.

Acceptance: a traceable sample lifecycle on each device, or precise unresolved blockers that bound the proposal.

## Phase 3 — SDK proposal and decision gate

- [x] Propose a bounded sample-bank resource API and explicit readiness/failure/lifetime policy below; general eviction remains deferred.
- [x] Specify ColdFire source/transfer jobs separately from DSP consumption; no file I/O or allocation in render.
- [ ] Specify playback cursor precision, signed rate, frame/channel units, start/end convention and completion.
- [ ] Specify interpolation guard reads, out-of-range behavior, gain/fades, PCM conversion and channel policy.
- [ ] Decide how selection and precise start/end editing fit the current four-word, single-page machine contract.
- [ ] Define voice stealing, retrigger, sample replacement and live parameter semantics.
- [ ] Specify state sizes, register/clobber rules, aliasing, rounding/error bounds and block contracts per kernel.
- [ ] Give a residency limit, polyphony target, memory budget and worst-case block-cost budget for both devices.
- [x] Present an initial phased proposal and API sketch below; detailed ABI and measured budgets are still open.
- [x] Em approves exact DSP extractions and numerical sampler primitives (2026-10-29).
- [x] Em approves the bounded sample service/transport and lifetime proof (2026-10-29).
- [ ] Em approves product-machine changes beyond the proof.

Proposed priority:

| Stage | Operations/services | Consumers |
| --- | --- | --- |
| First sampler | sample access/lifetime; bounded fractional cursor; signed rate; safe linear interpolation; PCM/channel handling; gain ramp and short fades | one-shot playback |
| Quality follow-up | cubic interpolation; explicit anti-alias/resampling policy | pitched playback; granular and wavetable |
| Later consumers | phase accumulator; table lookup; envelopes/windows; overlap mixing | oscillators, grains, wavetables |
| Driven by migration | noise; all-pass/comb; fractional delay; filter coefficient generation | existing machines and FX |

Interpolation quality and anti-alias filtering are distinct. Do not promise that linear or cubic interpolation
provides band-limited playback at high rates. Do not duplicate stock downstream filters/envelopes without first
checking what the module adapter already provides.

## Phase 4 — implementation (first milestone approved)

- [x] Implement exact reusable DSP extractions with reference/equivalence checks (approved).
- [x] Implement numerical sampler cursor/interpolation primitives with boundary tests (approved).

- [ ] Implement the smallest sample-service/adaptor changes and test ownership/failure behavior.
- [ ] Add minimum sampler primitives with independent numerical references and meaningful boundary tests.
- [ ] Build the one-shot sampler on both devices, with agreed selection and playback controls.
- [ ] Migrate one existing consumer at a time; preserve behavior and resource usage unless a change is approved.
- [ ] Update templates and authoring guidance to use the SDK operations.
- [ ] Add an enforceable dependency check for the agreed content-module boundary where existing checks lack it.

Do not expand core generated instruction semantics or regenerate emulator cores as incidental SDK work.
Any such requirement is a separate scoped decision with its full regeneration costs.

## Historical phased implementation scope

Em approved exact DSP extractions, numerical sampler primitives and the bounded loading/lifetime proof on
2026-10-29. This covers the DSP work in milestone 1 and the bounded proof in milestone 2 below. The read-only
graph-constructor tooling suggestion and product sampler remain proposals, not implementation approval.

Three milestones allow useful SDK work before the harder memory questions are solved:

1. **Exact DSP extraction.** Compare BD/PD float-phase and sine sequences and the BD/pluck xorshift paths;
   extract only operations whose semantics really match. Preserve arithmetic order, seeds, scaling and PCM.
   Keep freqshift's integer-turn oscillator separate. First integrate one consumer against its existing reference,
   then migrate the others after the cleanup checkpoint. Also add the small read-only graph-constructor option
   needed to prevent accidental `sharc.load()` rebuilds during analysis, with a focused read-only test.
2. **Bounded mono resident-sampler proof on both devices.** Add fractional cursor and boundary-safe linear read
   references, then a runtime-owned sample allocation/upload/publication path using existing DSP-link machinery.
   Start with a transferred native file below the existing 64 KiB whole-file limit; this is a proof limitation, not
   the final product sample-length promise. A packaged synthetic fixture tests the kernel but is not completion
   of the transferred-sample requirement. No continuous streaming, new transport or external DSP memory region.
3. **Product sampler integration.** Under the current requirements above, establish actual shared-pool capacity,
   format/length limits, chunked card reads, backing ownership and stereo integration. Then implement the user
   machine and migrate remaining modules with measured costs. Do not label the mono proof a finished sampler.

The approved DSP work in milestone 1 and the numerical part of 2 can run independently in separate writer worktrees.
Runtime sample transport and voice binding share contracts, so settle those contracts here before assigning
implementation. Run builds/gates serially under the repository's CPU budget; parallel static work is already cheap.

### Earlier general sample API sketch (future direction)

Names below are conceptual, not new exported ABI commitments:

- Control side: `sample_prepare(asset_id)` starts a bounded job; `sample_status(handle)` reports pending/ready/error;
  `sample_bind(machine, slot, handle)` publishes only a ready immutable descriptor at a block boundary.
- The handle includes a generation so old identities cannot silently refer to newly uploaded data. It is not a raw
  firmware pointer. Runtime/adapters own physical placement, accounting, transfer serialization and failures.
- DSP side: a resolved read-only view with frame count, channels, sample rate, encoding, channel stride and
  adapter-normalized address(es). Address representation must be explicit for SHARC's memory spaces. Resolve
  selection at note-on, outside the sample loop; no per-sample virtual dispatch, file I/O, allocation or lookup.
- First proof: resources remain pinned until restart, no eviction or live replacement. This avoids pretending a
  `DK_DONE` callback covers all voice termination paths. A reclaiming product API must cover stealing, retriggers,
  faults, idle and type changes in the adapter before memory can be reused.
- Keep the sample reader independent of storage encoding; compare packed int16 with float32 on the target rather
  than committing all samples to float32 and halving capacity without measurement.

### Proposed initial playback semantics

- Frame interval is half-open `[start, end)`; forward starts at `start`, reverse at the last included frame.
  Empty intervals produce a fully written silent block and completion. Never read an interpolation neighbor beyond
  the interval; a proposed first policy clamps the neighbor at the edge. Tail samples after completion are zero.
- Cursor uses a separate integer frame index and fractional phase (precision selected with boundary/rate tests),
  avoiding an ever-growing float32 absolute position. Signed increment controls direction and note pitch; source
  rate and root-note metadata determine the ratio. Pitch range and anti-alias quality remain explicit decisions.
- Latch sample identity, bounds and direction at note-on for the first proof. Define live automation separately;
  changing a pointer mid-block is not an acceptable implementation of sample selection.
- Reuse stock downstream amp/filter behavior; any tiny boundary fade is separately specified and tested, since
  even a click-prevention fade changes the waveform. Test hard endpoints and faded endpoints independently.
- Proposed four-word layout: sample slot, start, end, reverse, with pitch from the existing voice input. This fits
  the current parameter transport but cannot provide precise frame editing of long clips with 128 knob values.
  Exact trim controls remain open; the on-device picker and full stereo are now required. No silent downmix is approved.

### Acceptance before expanding the proof

- Same mathematical cursor/reader contract on both devices; synthetic exact vectors for one/two-frame samples,
  fractional rates, reverse, bounds, completion and block continuation. Numerical quality is separate from timing.
- A transferred sample actually becomes audible through the machine contract on each emulator. Invalid/truncated
  files, full allocation, transfer failure and selecting an unready asset produce a defined failure or silence.
- Account for all code/state/sample bytes. Plan for 16 concurrent voices and simultaneous triggers; do not inherit
  a conformance assumption that only one note-on consumes the block headroom.
- Report instruction ceilings and one paired A/B for any speed claim. Hardware performance/cache coherence remains
  unproved until an explicitly authorized device test. No new absolute CPU budget; the historical ten-second
  fixture does not define the product sample-length limit.

## Historical approved proof — loading and lifetime

Em approved this bounded bank API on 2026-10-29, narrower than the earlier general prepare/bind/release sketch.
Evidence and exact current-image mappings are in finding 105. Runtime implementation is integrated below;
the transferred-file bounded emulator proof passes on both current OS images. The first SDK milestone is checkpointed at `4cd29a3c`.

**Scope:** prove an actual transferred mono sample can reach the existing numerical reader on both devices,
with correct ownership and failure behavior. Preserve the current Transfer destinations. No new transport,
stereo ABI, eviction, or new sample browser in this proof. Borrowing the DT2 stock pool is the chosen direction
(finding 109) but is outside this proof.

**Resource declaration:** a versioned SDK sample-bank declaration associates a module-local bank id with
its DSP BSS capacity and initialized descriptor symbol. The packer resolves and validates offsets within the
owning module's DSP data/BSS; runtime accounts the capacity in that module's existing allocation. Modules do
not submit arbitrary DSP addresses. Exact record encoding/version compatibility is an implementation design
task within this scope, to be reviewed before the packer and runtime workers diverge.

**Control interface:** conceptual `dk_sample_load(bank, path)` requests one job and `dk_sample_status(bank)`
reports queued/loading/ready/error. The bank declaration identifies the owner, which must already be fully
loaded. Calls marshal into the UI task; they do not mutate the DSP queue from a MIDI callback. The path uses
the device source adapter (DT2 +Drive, DN2 digikit store). Missing support reports a capability/error result.
Only an unused bank accepts a request; repeated or concurrent requests report BUSY/ALREADY_LOADED instead of
overwriting an active view. Once a transfer starts, no destination reuse in this proof, even after failure.

**Lifetime:** one job, one module-owned bank, pinned until restart. Retriggers/voice stealing reset only voice
cursors. A failing sample job must not invoke module-load rollback. READY starts zero in initialized DSP data;
bytes transfer first, immutable descriptor fields next, READY last, each phase acknowledged. A lost final
acknowledgement may mean publication already occurred, so pin the bank regardless of the control-side error.
Never reclaim the retained ColdFire transfer buffer until the link is demonstrably quiescent. After timeout or
cancel, report failure but keep that source quarantined if necessary. Shared transport cleanup changes are in
scope only where the new lifecycle tests demonstrate they are needed.

**Proof limit:** one 32 KiB float32 bank = 8,192 mono frames at 48 kHz (~171 ms); code, descriptor and voice state
are additional. Input native file at that frame limit is 16,464 bytes including header/trailer. This keeps the
proof below the existing whole-file cap and uses the already-tested reader. The declaration is capacity-based;
this is a proof configuration, not a universal SDK or product length limit. Fit must include other modules.
Reject stereo and oversize input explicitly. Final packed PCM format and longer clips remain separate decisions.

**Source copy:** lookup/read/validate the complete native file into private ColdFire storage before asynchronous
delivery; do not retain shared staging. On DN2 require stable metadata plus matching content hash; missing
fingerprints or exhausted retries fail closed. On DT2 open only in read mode, use size(entry id), verify counts
and format, and close every successfully opened handle. First emulator proofs exclude simultaneous Transfer
writes; do not claim concurrent DT2 overwrite/delete support until its handle behavior is tested or an enforceable
exclusion exists. Bound work per UI pass; synchronous read latency must still be measured.

**Capacity and replacement follow-up:**

- [x] Measure no-dynamic-module and proof-only runtime capacity on both current images; persist provenance and limits.
- [x] Record the user's stop-old-notes-on-selection choice and distinguish temporary CF source from pinned DSP storage.
- [x] Record the historical ten-second prototype target; superseded by the current stereo/shared-pool requirements. Stop old notes on selection remains required.
- [ ] Borrowing the DT2 stock pool is the chosen direction (finding 109). Prove slot ownership, external mutation interception, lifetime/reclaim and cache rules
  in the adapter before borrowing stock backing. The terminal DN2 owned-bank proof does not establish stock backing ownership.
- [x] Prove bounded DN2 emulator backing/delivery for 960,000 packed PCM16 bytes and full ten-second playback,
  with exact payload, complete canaries, failure cases, actual dispatch, end cursor and early/late PCM evidence.
- [ ] Establish hardware ownership/cache/coexistence and general capacity beyond that isolated emulator range.
- [x] Record proposed retirement sequencing and source-survey/review results in finding 83; transport ACK is not a reader barrier.
- [x] Implement and prove the scoped terminal DN2 retirement contract: guarded note_on/render admission/drain, closed cached/delayed callbacks, held/retriggered starts, exact/stale ACKs, pending retries and timeout.
- [x] Prove DN2 generation reopening, sealed B skip, C playback, stale controls and serialized writer failure-stop. Concurrent queued loads are covered by host tests; the coupled case executes A → sealed B → C. Strict event-age rejection is not the selected policy.
- [ ] Establish product stop granularity and a shared public ABI after the DT2 lifecycle proof.
- [x] Prove the packed PCM16 reader and fixed resident diagnostic consumer numerically and on coupled DN2.
- [ ] Measure paired cost before a performance/adoption claim; product sample-picker scope remains open.

**Approved proof work:**

- [x] Agree declaration/record and status contracts here, then assign disjoint SDK/packer and DT2 source-adapter
  tasks to workers in separate worktrees. One worker owns shared runtime queue/lifetime integration.
- [x] Add host tests for declaration ownership/bounds, private snapshot consistency and fake-link state transitions.
- [x] Add current-image DT2 read-only filesystem bindings/wrapper and DN2 snapshot reader.
- [x] Add module-owned bank job and immutable descriptor publication, serialized with existing module/registry work.
- [x] Add a minimal proof consumer on both devices using actual transferred data; bound emulator runs and test PCM,
  silence-before-ready, oversize failure and retrigger behavior. Host tests cover allocation capacity/failure;
  emulator acceptance does not establish arbitrary module coexistence. No user sample audio committed.
- [ ] Report latency, memory and instructions; review product sample length, stereo and editing scope separately.

Independent evidence audit supports the source constraints, including UI task placement, conditional early-free
risk, private-snapshot requirement and per-operation filesystem locking. It does not establish hardware cache
coherence, file read latency, concurrent DT2 replacement safety or the adverse timeout schedule on hardware.

## Validation and performance

The current audio contract is 32 frames at 48 kHz: approximately 667 microseconds for the entire audio block.
This is a deadline, not measured free time for a module. Establish stock load and reserve headroom on both devices.
Emulator instruction counts do not establish hardware cycle timing or model all pipeline/cache/DMA stalls.

- [ ] Numerical and boundary tests: empty/short samples, endpoint reads, forward/reverse, fractional rates,
  stereo, retrigger, replacement, missing samples, state across blocks and non-finite outputs.
- [ ] Meaningful reference vectors and memory guard checks for new kernels; preserve old PCM pins unless Em approves.
- [ ] Cost ceilings first: per voice/block, worst-case transitions, aggregate voices/FX, state and buffers.
- [ ] One paired A/B with `tools/devtools/coupled_ab.py` for performance claims, with identical workload and provenance.
- [ ] Assess ColdFire loading/conversion, cross-core transfer, bandwidth and underrun risks separately from arithmetic.
- [ ] Hardware timing proof only after explicit authorization; distinguish emulator evidence from hardware evidence.
- [ ] Use owner's iteration checks, then routed integration gate once before merging; no parallel gate/build.

SHARC review covers scheduling, multifunction instructions, register pressure, DAG hazards, loops, SIMD suitability,
alignment, bank/bus access and state placement. Cache/prefetch/DMA changes require an established memory path and
ownership/coherency protocol. Hoist conversions/coefficient work out of sample loops where semantics allow it.

## Evidence and progress log

### 2026-10-29 — resumed source/lifetime discovery

- [x] Resume from `4cd29a3c`; preserve clean implementation checkpoint and isolated worktree.
- [x] Run disjoint read-only scouts for DT2 source acquisition and runtime/store lifecycle.
- [x] Port DT2 filesystem candidates by strict function hashes, then read current Ghidra dossiers. Correct the
  initial mistaken nearby-address inference and worktree input-link gap; record only the actual mapped entries.
- [x] Inspect UI job context, source snapshots, allocator ownership, transport source retention and READY ordering.
- [x] Obtain independent evidence audit; record caveats and the bounded next implementation proposal in finding 105.
- [x] Em agrees to the loading/lifetime proof scope above before core runtime/adapter or packaging edits (2026-10-29).

### 2026-10-29 — approved loading/lifetime implementation in progress

- Source worker: `/private/tmp/digikit-sample-source`, branch `refs/heads/sdk/sample-source-proof`.
- SDK/packer worker: `/private/tmp/digikit-sample-bank`, branch `refs/heads/sdk/sample-bank-proof`.
- Runtime worker: `/private/tmp/digikit-sample-runtime`, branch `refs/heads/sdk/sample-runtime-proof`.
- All lanes start at `a3a95073`; parent integrates into this plan's worktree. No edits in the cleanup checkout.
- Contract settled: optional one-bank DSP section v5, existing 64-byte big-endian header with bank-record offset
  at byte 60; 32-byte little-endian record (version 1, local numeric id, BSS byte offset, capacity in frames,
  initialized descriptor byte offset, three zero reserved words). Capacity 1–8,192 frames for this proof.
- DSP descriptor is four initialized zero words: READY, normal-word source pointer, frame count, sample rate.
  Runtime publishes READY last. Both packer and runtime validate ownership, bounds and initialization.
- API minor 1.3 appends `sample_bank`, `sample_load`, `sample_status`. Lookup during init yields an opaque handle;
  load queues a copied path, and execution waits for the owner's full DSP load. Status 0 unused, 1 queued,
  2 loading, 3 ready, negative existing SDK error codes on failure. No destination reuse after transfer starts.
- [x] Integrate source, packaging and lifecycle implementations; review the combined boundary behavior.
- [x] Add the proof consumer and transferred-file emulator scenarios; execute owner checks on both devices.
- [x] DN2 1.12: run the transferred-file scenario through oversize failure, clean restart, unready dispatch/silence,
  exact READY publication, replacement refusal, audible playback/retrigger and no-fault checks.
- [x] DT2 1.17: run the same transferred-file scenario with the current-worktree daemon.

Implementation and bounded-proof checkpoint (product scope remains open):

- Source, SDK/packer and runtime patches are integrated in the parent worktree, uncommitted. Decoder/snapshot
  tests pass (five); lifecycle tests pass (seven), including delayed pointer reads and lost final READY acknowledgement.
  Worker validation: patchkit 127, dkneeds 11, SDK generation 8 tests passed; host and actual ColdFire builds passed.
- New `test/sample-proof` fixture and `sample-playback.test.ts` in `tests/regression/runtime/` are integrated. Six assembled
  consumer tests pass. The fixture packs for both devices. Device playback remains unproven until the TS scenario passes.
- Parent SDK tests pass (47), and fit tests pass (nine). `module:check test/sample-proof` reports 29 passed and five
  skipped in total; package/fit covers both devices. These checks do not establish transferred-file device playback.
- The proof bridge preserves I12/I13 and adjusts I7. Harness tests now use frame zero at the actual DN2 adapter
  entry; the stock helper's frame value 18 applies only after its return.
- Generation initially omitted all cached content declarations. A scoped fix now reads existing cache objects in
  lenient generation without compiling, with a passing regression. Fixture gallery was generated after packing.
- Independent combined review found no new issues. Existing blocking module-row cleanup can still ignore failed
  writes; the new quarantine protects the accepted module-image source but is not a universal rollback safety proof.
- DN2 emulator first failed on sandbox socket permission; escalated local emulator execution is authorized. The
  subsequent optional `cf.*` import/mandatory-anchor failure is resolved: per-device function tables contain the
  four DT2 path bindings and DN2 zeros, preserving generated symbol anchors. Runtime artifacts are rebuilt.
- DN2 boots with an empty module selection; selecting the proof module exposed `UnsupportedGuestPc(0)` before
  sample loading. Diagnostics identified context restoration by RTE at `0x40000458` after stack corruption.
  The confirmed cause is an emitted weak compiler-builtin `__bswapsi2` that calls itself: its return address
  `0x4673d290` and argument `0x00200000` match the corrupted ColdFire registers. The new `valid_bank` call to
  `from_le_bytes` triggers this before module init. The initial static init audit had found no error.
- `rust.rs` and the new `cf_rust_helpers.s` in `packages/patchkit/src/build/` now provide a strong
  private ISA_C BYTEREV cdecl helper before the builtin archive; the helper source text is included in the build
  hash. Runtime rebuilt with 33,384 instructions; the helper is eight bytes (`202f000402c04e75`) with no outgoing
  relocations. Build log: `out/runs/dsp-sdk-checks/cf-helper-runtime-build.log`.
- Worker regression `coldfire_rust_endian_conversions_return_without_recursive_helpers` in
  `packages/patchkit/tests/build.rs` passes: actual cross-compiled endian reads/writes on five inputs run on
  `ColdFireCpu`, bounded at 100 instructions, checking cdecl stack/kept registers and private helpers with no
  outgoing relocations. It ran release/offline in `out/native/target-wt/digikit-dsp-sampler/nogen`.
  Independent review found no issues. MCF5441xRM printed page 97, Table 3-4 confirms BYTEREV's operation,
  matching emulator behavior; this is not hardware proof.
- `packages/emulator/coupled/src/cold.rs` now writes bounded `boot-fault.json` diagnostics on guest faults,
  preserving the original error and capturing registers, at most 128 stack bytes, the last recorded bus PC and
  at most 32 code bytes. The rebuilt daemon uses mandatory `--coupled`; build log:
  `out/runs/dsp-sdk-checks/diagnostic-daemon-build.log`. Selected-fixture DN2 boot now passes; log:
  `out/runs/dsp-sdk-checks/sample-dev-dn2.log`. This is boot evidence, not a performance or sample-playback claim.
- First DN2 TypeScript run (session 6653) passed module upload/reboot, oversize rejection (-13), zero descriptor,
  no-fault and fresh-boot checks, then failed unready audio after selecting MIDI row 4. This navigation failure
  does not establish a DSP playback result. Live panel inspection confirmed four audio rows, MIDI, then SAMPLE
  PROOF at row 5; the corrected selection shows the fixture's START/END/REV controls.
- The scenario now selects row 5, closes the modal with NO, saves framebuffers, and asserts the adapter's actual
  `_dv_c` type from the patch plan equals the runtime-assigned type before testing unready silence. Session 39860
  failed that assertion (expected type 16, actual 0), despite unready peak zero. The saved panel was still in the
  machine menu on MIDI: a fixed 500 ms wall wait raced queued keys, so silence did not establish fixture dispatch.
- The selector now waits on `inputTap.release_at` and `status.elapsed`, polling every 50 ms with a 20 s bound;
  missing clock fields fail clearly. It mirrors the existing harness: 20 ms hold, 5 ms pre-scan/release,
  200 ms open, 80 ms navigation and 300 ms YES. Parent's known-panel NO → menu → row 5 → YES sequence confirmed
  SAMPLE PROOF START/END/REV. Completed DN2 reruns now confirm actual type 16 dispatch and exact-zero unready
  PCM, followed by valid-load error -35 instead of READY. Artifact `run-15D8JZ/publication-state.json` records
  `dl_link` state 3, error 100 (timeout), acked 0, and an unpublished descriptor. The failed timing log remains
  `out/runs/dsp-sdk-checks/sample-playback-dn2-input-timing.log`, artifact run `run-F9Pyr7`.
- Fast `dk:dev` reproduction isolates `native-trap: no decoded instruction at 0x254aa1` on the first TRIG 1,
  before sample transfer: `out/runs/dsp-sdk-checks/sample-note-dn2.log`. The later link timeout is likely
  downstream of that DSP halt; this is an inference, not a transport root cause. Whether the note fault is in
  the module integration or emulator remains open. Numerical and conformance passes do not resolve it.
- Bounded live PCDUMP traces pin the bad return to the second `DK_CALL` delay-slot store: Type 3b `u=0`
  pre-modify leaves I7 unchanged and writes zero at I6-8; the expected I6-4 return slot retains `0x254aa0`.
  The kernel epilogue reads that stale value into I12, then jumps to `0x254aa1`. Evidence:
  `out/runs/dsp-sdk-checks/unready-module-trace-window/pcdump.csv` and
  `out/runs/dsp-sdk-checks/unready-stack-alias-trace/stack-rows.json`. Public PRM pages 13-18/13-19 confirm
  `u=1` denotes post-modify. The Python test path also used an abstract call stack: an unknown saved return
  bypassed target checking and returned through recorded call metadata. This explains the earlier false pass;
  no interpreter/native core semantics were changed.
- Approved bounded correction: `DK_CALL(fn, return_label)` now requires a caller-supplied unique label and
  emits an immediate Type 16a post-modify store of `return_label - 1`. The one-argument form is rejected. All
  repository SDK callers were migrated. Repeated/nested-call experiments proved distinct relocations with
  addend -1; neither numeric local labels nor a builtin `__COUNTER__` was usable, so no preprocessor feature
  or dependency edit was added. Artifacts: `out/runs/dsp-sdk-checks/call-label-experiment/`.
- New `test_dsp_call_frames.py` in `tests/patchkit/` executes the actual assembled bytes in Python architectural-stack
  mode and the fingerprint-checked native core, with runtime decoding, poisoned saved-return slots, repeated
  and nested calls, low-alias and L2-byte-alias stacks. It asserts actual continuation PCs, saved frame/return
  slots and exact I6/I7 restoration; old one-argument assembly must fail. The four named suites (call frames,
  generic kernels, sample kernels, fixture consumer) passed **85 tests, no skips** with an explicit current native
  library. Log: `out/runs/dsp-sdk-checks/sdk-call-frame-tests.log`.
- Actual fixture `module:check` passes **29 tests, five skipped**, and both device packs were refreshed under
  `out/regression/modules/{dn2,dt2}/sample-proof.dkm`. Each needs 33,984 DSP bytes and fits an empty device;
  this does not establish coexistence or device playback. Logs: `out/runs/dsp-sdk-checks/sdk-call-module-check.log`,
  `sdk-call-module-build.log`, `sdk-call-pack-dn2.log` and `sdk-call-pack-dt2.log` in the same directory.
  The cached fixture now decodes its return store at byte 396 as Type 16a, with an ABS32/addend -1 relocation
  to `_sp_after_linear` (parcel 201).
- Parent's corrected fast DN2 `dk:dev` first-note reproduction now passes without a DSP halt (12.38 s total):
  `out/runs/dsp-sdk-checks/sample-note-fixed-dn2.log`. This is a bounded first-note check, not full transferred
  playback acceptance by itself. The subsequent full DN2 scenario **passes** (session 7984), with one test
  passed and one DT2-filtered skip, 235.36 s total wall time and 228.197 s scenario time:
  `out/runs/dsp-sdk-checks/sample-playback-fixed-dn2.log`. It verifies uploaded 9,000-frame oversize rejection
  (-13), zero descriptor/no fault, clean restart, actual type 16 dispatch, exact-zero unready PCM/no fault,
  4,096-frame exact decoded publication with READY=3, replacement refusal (-10), audible playback/retrigger
  and no faults. These suite durations are not loading-latency or performance measurements. Passing temporary
  artifacts at `run-fMhyJt` were removed by normal test cleanup; exact ready/retrigger peaks are not retained here.
- Independent review found no concrete issues in the explicit-label macro or native return-frame regression.
  DT2 current-worktree daemon build completed in about 90 s (session 47443), logged in
  `out/runs/dsp-sdk-checks/sample-dt2-daemon-build.log`. The prepared binary is
  `out/native/target-wt/digikit-dsp-sampler/g-e7aa50f9/release/digi-emud`, used for the passing DT2 retry below.
- The first DT2 scenario retry failed its diagnostic assertion (expected type 16, read 1), while screenshots
  showed SAMPLE PROOF with START/END/REV and the DSP remained healthy. The test had read DN2's diagnostic
  layout: DT2 stores type at `_dv_g + 4` and `_dv_c` word 0 is note-on; DN2 stores type at `_dv_c` word 0.
  The scenario now resolves the device-specific symbol from its exact patch plan and records the address.
  The exact assigned-type assertion is retained; this test-only correction resolves the failure.
- DT2 1.17 retry **passes**: one test passed, one DN2-filtered skip, 94.25 s wall time and about 67 s scenario
  time (`out/runs/dsp-sdk-checks/sample-playback-dt2-retry.log`). It verifies the same assertions as DN2:
  oversize -13/zero descriptor/no fault, clean restart, actual type 16, exact-zero unready PCM and healthy DSP,
  exact READY=3 bank bytes, replacement -10, audible finite playback/retrigger and no faults. Both-device
  bounded proof acceptance is complete; these durations are not loading-latency or performance measurements.
- Focused stock OneShot investigation is now requested. Current DT2 1.17 database candidates are render
  dispatch `0x1c6b00` → `0x1c4ecf` and stretch branch `0x1c6af1` → `0x1c5576`; stock Type 16a immediate
  post-modify return stores provide a comparator. These are [D] database observations, not [V] byte audits
  or a completed sample-access/ownership trace.
- The TypeScript scenario now records `unready-state.json` (daemon status, faults and PCM summary) immediately
  after capture and calls `expectNoFault('unready proof-machine note')` before valid loading. Publication
  diagnostics also retain daemon status/faults. Formatting and TypeScript checks pass, and the full DN2 run
  above and the DT2 retry now exercise these assertions successfully.
- Partial `test:changed` checks pass Ruff, layercheck and the two selected Python files
  `tests/devtools/test_changed_tests.py` and `tests/devtools/test_verify.py`. The known baseline ten stale
  references still block repository pathcheck. Host checks were blocked by pnpm 12's automatic install attempt
  and unavailable offline cache; no dependency changes were made. The later owner host related-test command
  exits zero with no selected host tests (`out/runs/dsp-sdk-checks/sample-host-related.log`); oxlint passes
  using existing Node binaries. Neither is a full gate pass. Changed-check log:
  `out/runs/dsp-sdk-checks/sample-test-changed.log`.
- The subsequent SDK-call changed check passed Ruff and layercheck but reported eleven path references.
  The added temporary-test path in `packages/patchkit/src/module.rs` is now constructed with separate `join`
  calls (same resulting path, no production change). Repository pathcheck confirms the remaining ten known
  baseline references in five unchanged files; log: `out/runs/dsp-sdk-checks/sdk-call-pathcheck-after-test-path.log`.
  Scoped checks of that source file and these documents pass. The full routed gate has not run.
- Next: use the executed replacement observation in finding 82 to design and prove explicit consumer retirement
  before overwrite/release, including external slot replacement/reset and stale queued notes. Preserve the existing
  immutable-bank proof and DN2 playback seam; larger DN2 backing/delivery remains a separate missing contract. The confirmed target is approximately ten-second
  one-shots, stopping old notes on selection. Backend capacity/lifetime and sample-picker work remain unimplemented. The current limit is 8,192 mono frames at 48 kHz
  (about 171 ms), with session-pinned immutable source storage and fixed proof paths. Continue stock-reader
  investigation separately; no stock-quality equivalence, coexistence, performance or hardware claim follows.
- A standalone native-boot diagnostic halted after six instructions, before reaching the target. It is unusable
  for this investigation and does not establish proof behavior.
- Runtime artifact builds require rustup Cargo first in PATH (`/Users/em/.cargo/bin`), not Homebrew Cargo.
- Parent shares only input symlinks; Node workspace package links resolve to this worktree, dependency cache is shared.
  Ready inputs' firmware hashes match current DT2 1.17 / DN2 1.12, despite an old DN2 image label in provenance.
- Local check logs: `out/runs/dsp-sdk-checks/`. The parent completed the single `runtime/sample-playback.test.ts`
  scenario on DN2 first and DT2 last, with bounded sessions and shared benchlock.

This approved continuation completes the bounded sample API, proof fixture and both-device emulator playback
acceptance. It does not resolve general conditional transfer cleanup or establish hardware behavior, production
capacity, arbitrary module coexistence, stock quality or performance. Use git log for the final checkpoint commit.

### 2026-10-29 — first implementation checkpoint

- Two workers used isolated writer worktrees based on `177bec02`; the parent integrated their scoped changes.
  Original cleanup checkout remained at `219570ca` when checked; final cleanup reconciliation remains pending.
- `sdk/dsp/noise.inc`: 6-instruction preloaded-shift and 7-instruction inline-load xorshift schedules.
  BD and pluck now use these macros without changing seeds, conversion or surrounding instructions.
- `sdk/dsp/oscillator.inc`: exact 16-instruction folded-phase polynomial, used by BD and PD. The preliminary
  spoken estimate of 17 instructions was corrected by counting the actual sequence. Phase folding/warping stays
  in the consumer. All register aliases and clobbers are explicit.
- Complete assembly comparison against `177bec02` found identical executable bytes, relocations, symbols and DM
  for BD (1,056 PM bytes), PD (1,446) and pluck (1,328). Local proof: `out/runs/dsp-extract-proof/`.
- `sdk/dsp/sample.inc`: 5-instruction integer cursor update plus bounded 32-sample mono float32 reader. State is
  start/end/index/fraction/signed Q16.16 rate/done. It clamps the interpolation neighbor, never reads an exhausted
  interval, zeros the output tail, and carries exact split position across calls. This is not a storage/resource ABI.
- Numerical limitations: quantized Q16.16 rates, mono immutable float32 view, mapped source interval, no overlap,
  no playback looping/fades/anti-alias filter. Large indices beyond float32 integer precision are tested using
  sparse valid mapped addresses; a maximum-range cursor case stays reference-only because SHARC address windows
  are segmented. A numeric base+end bound alone does not establish a valid memory mapping.
- Workers ran 13 oscillator/noise tests and 45 sampler tests successfully. Sampler coverage includes independent
  combined-position arithmetic, reverse/zero/extreme rates, two-block continuation, done/tail behavior, source-read
  guards and callee-saved registers. Pure reference tests run even without an assembler.
- Sample-reader test ceiling: 1,380 instructions including caller argument loads; the two-call harness measured
  557–2,231 against its 2,761 ceiling. No hardware cycle, throughput or speedup claim.
- Independent read-only review found no concrete issues in either patch. Parent integration checks and module
  owner checks are recorded in the validation section below; a reviewer inspection does not replace them.

### 2026-10-29 — parallel discovery and independent audit

- [x] Run three disjoint read-only investigations: DT2 stock sample path, DN2 delivery/memory, and general DSP on both.
- [x] Parent audits voice lifecycle/format and uses existing SQLite-backed NetworkX call graphs for both target images.
- [x] Independent evidence auditor checks DN2 payload/capacity arithmetic, mono ABI and DT2 page-copy listing.
- [x] Record source/listing results and unresolved lifetime/coherency questions in finding 104; index it in `docs/FINDINGS.md`.
- [ ] Independently decode relevant original instruction bytes before using [V]; matching the DT2 blob hash alone is insufficient.

Corrections caught during integration: filling the same DSP byte area takes the same transfer time for mono and
stereo (duration differs); DT2's page copy is scalar head/tail plus SIMD pairs, not 512 scalar words; the slot-reader
valid branch is a separate database function fragment. DN2's 64 KiB public read limit includes the native file's
header and trailer. No implementation or performance measurement was performed.

Parent source comparison confirms a first exact extraction candidate: BD/pluck share xorshift shifts 13, -17, 5;
their float conversion paths differ and should initially remain with the caller. BD/PD share sine coefficient words
and polynomial operation order after folding, but their phase/modulation code remains distinct. Preserve scheduling
and rounding first; optimization is a later measured change. Finding 104 records this narrower scope.

### Initial module inventory (pinned baseline; source inspection, not execution)

All five include the SDK machine/FX ABI. None currently includes the DSP kernel library. The four machines import
the adapter's `_dv_exp2`; review its public SDK exposure before deciding whether to wrap or replace it. A runtime
import is not itself a hard-coded firmware address. The existing SDK's similar-sounding operations are not
automatically equivalent to the algorithms below.

| Module | Extraction candidates | State and behavior to preserve | Evidence/checks |
| --- | --- | --- | --- |
| pluck | noise/excitation, one-pole, fractional all-pass, gain | 16,512 bytes per voice including 4,096-sample line; Karplus–Strong tuning, pick comb, damping and quiet-note termination | `modules/machines/pluck/src/dsp/pluck.s`; `module:check digikit/pluck`; module-load acceptance |
| bd | phase stepping, sine approximation, noise and envelopes | 32 bytes per voice; carrier/modulator phase and counter reset, falling FM index | `modules/machines/bd/src/dsp/bd.s`; `modules/machines/bd/tests/scenarios/equiv-dyn-bd.toml` pins per-device PCM |
| analog | oscillator phase/waveshapes, noise, gain | 24 bytes per voice; free-running phases; reflective wave folder is not the SDK soft clip | `modules/machines/analog/src/dsp/analog.s`; owner module check after cleanup reconciliation |
| pd | phase stepping, polynomial sine, phase warp, slew | 36 bytes per voice; reset/bootstrap/slew and folding semantics | `modules/machines/pd/src/dsp/pd.s`; owner module check after cleanup reconciliation |
| freqshift | oscillator and polyphase all-pass stages | 664 bytes of state; preserve phase/quadrature and all-pass operation order | `modules/fx/freqshift/src/dsp/freqshift.s`; float32 reference and device scenarios described in `modules/fx/freqshift/README.md` |

BD, analog and PD declare a block instruction ceiling of 1,200 and note-on field of 200 in their descriptors.
These are source declarations, not measurements; the generated ABI describes note-on cost as note-on plus render,
so verify the checker/adapter interpretation before using those declarations as performance evidence.
PLUCK's generic delay cannot simply be substituted for its tuned feedback loop; freqshift's all-pass network cannot
be substituted with the existing biquad merely because both are filters.

Tentative migration order: extract exact shared phase/sine/noise operations where their semantics match, then one
small oscillator consumer, then the remaining oscillators, then pluck and freqshift. Start with the consumer having
the strongest existing reference proof after cleanup lands. The new sampler remains the priority for new APIs;
its linear interpolation and channel behavior belong in the initial contract, not an unspecified later extension.

### ABI and capacity observations

- Machine v1 passes pitch, velocity, block index and a mono output buffer; it exposes note-on and render, but no
  sample view, explicit channel identity, release callback or sample-resource lifetime hook. An adapter or ABI
  extension must account for those requirements; no extension has been chosen.
- FX v1 uses 64 planar floats (32 left then 32 right). Machine stereo behavior therefore needs explicit investigation;
  do not infer it from the FX contract. Definitions: `sdk/sdk.toml`, `sdk/include/dk/gen/machine.h`.
- Device profiles reserve 524,288 DSP bytes on DT2 and 327,680 on DN2, shared by loaded module code/state. These
  are not established sample-pool limits. The profiles explicitly say their 1 GHz clock is an emulator assumption,
  not a known hardware clock: `devices/digitakt-ii.toml`, `devices/digitone-ii.toml`.
- Whole-file `store_read` is bounded both by caller capacity and the runtime staging-buffer limit; a future sample
  service must not assume it can load arbitrary transferred files through that API.

### Query setup and next trace

The worktree reuses only the existing `out/cfdb/`, `out/sharcdb/`, `out/atlas/`, `out/refs/` inputs and `.venv` by
symlink. Firmware query databases are opened read-only; the worktree has its own `out/cache/` and run outputs.
Use `UV_NO_SYNC=1` and a worktree-local `UV_CACHE_DIR` when invoking uv, so dependencies are not synchronized.
Both current-image ColdFire, SHARC and atlas databases are present; image-byte provenance checks remain open.

First `fw batch` queried sample atlas entries on both images and the SHARC sample-name catalogs. The name search
does not identify a DSP sampler; this is not evidence that no relevant code exists. Next inspect the sample-loader
class and its callers/dispatch on both images, then follow data references and the historical DT2 LP0 route.
Keep symbol-name matches as candidates until their actual behavior is established. No DB rebuild was requested.

### 2026-10-29 — initial inspection

- SDK kernels and their known scheduling limits: `sdk/dsp/README.md`.
- Numerical references and kernel checks: `tools/patchkit/dsp_ref.py`, `tests/patchkit/test_dsp_kernels.py`.
- Machine inputs/entry points currently lack a sample view: `sdk/include/dk/gen/machine.h`.
- Storage distinction: `docs/design/digikit-store.md`; whole-file ColdFire read:
  `modules/core/runtime/src/rs/src/store.rs`.
- Historical stock sample path: `docs/findings/15-sharc-sample-path.md` (DT2 1.16, not current-image proof).
- Current-image kernel study: `docs/findings/72-sharc-kernels-1-17.md` (static [D]/[O], not cycle measurements).
- Existing graph layer: `tools/sharc_dev/sharc.py`; graph communities: `tools/firmware/catalog.py`.
- Public manuals and provenance: `docs/sharc/SOURCES.md`; extracted reference text lives in `out/refs/`.
- Initial read-only scout confirmed the missing public sample-access contract. No firmware address has been
  newly verified, and no build/emulator/hardware run has been performed for this discovery.

## Plan maintenance

Validation (2026-10-29, historical resumed discovery documentation, before proof approval):

- Targeted pathcheck for the plan, finding 105 and findings index passes; `git diff --check HEAD` passes.
- `mise run test:changed` passes layercheck and fails only on the same 10 stale references in five
  unchanged baseline files recorded below. No code, firmware or hardware checks were needed for this
  documentation-only checkpoint. The runtime proof was awaiting agreement then; Em subsequently approved it.

Validation (2026-10-29, first SDK implementation):

- Parent integrated suite: `uv run python -m pytest -q tests/patchkit/test_dsp_osc_kernels.py
  tests/patchkit/test_dsp_sample_kernels.py tests/patchkit/test_dsp_kernels.py`: **81 passed**.
- `mise run module:check -- fadedlimes/bd`: **15 passed, 7 skipped**; package/fit passes DT2 and DN2.
- `mise run module:check -- fadedlimes/pd`: **22 passed, 5 skipped**; package/fit passes DT2 and DN2.
- `mise run module:check -- digikit/pluck`: **42 passed, 5 skipped**; package/fit passes DT2 and DN2.
  Five skipped cases are the suite's unselected negative fixtures; BD also has two skipped equivalence scenarios.
  These results are not a new all-module load or full firmware-device proof.
- `mise run lint:py` passes (lint and formatting). Targeted changed-document/include pathcheck and
  `git diff --check HEAD` pass.
- `mise run test:changed` passes changed Python lint/format and layercheck, but the repository-wide pathcheck
  retains 10 pre-existing references in five unchanged files (DSP recipes and web transfer/test files). No unrelated
  fixes were made; reconcile these with the cleanup session. Local logs: `out/runs/dsp-sdk-checks/`.
- Independent reviewer found no concrete issues. Full routed merge gate is still required before merging;
  it was not run for this implementation checkpoint. No hardware run or cycle/speedup measurement.

Documentation checkpoint: `177bec02`. The first SDK implementation is checkpointed separately on this branch;
use git log for its commit. Module source instruction/data identity and numerical tests support this scoped
checkpoint; sample transport/lifetime, product controls and stereo remain open.

Check an item only when its acceptance criterion is met; record evidence and residual uncertainty alongside it.
Do not mark a proposal implemented or a historical finding current. Update this plan after each meaningful
discovery or implementation checkpoint. Keep new questions visible and record accepted decisions with dates.


### 2026-10-29 — packed PCM16 checkpoint and isolated transport preflight

Em's “Go for it” approves the bounded ten-second diagnostic and numerical prerequisites;
a product sample/resource API remains outside that approval. Packed PCM16 reader checkpoint
`6ece2ae8` has 96 passing numerical/assembled tests and completed parent review. The float
reader remains unchanged; the new reader uses packed signed16 containing words and the
existing exact split cursor.

The next isolated transport stage is `core/dsp-ddr-proof`, selected only by explicit
`apply --modules core/dsp-ddr-proof`, conflicting with `core/dsp-link`. Its full-frame mailbox protocol is
[documented with the module](../../modules/core/adapters/dsp-ddr-proof/README.md). It is a
quiet host-driven CF→SPI→DSP diagnostic, without a runtime sample ABI, event-drain claim,
live-audio loading or production machine. The candidate emulator range holds exactly
960,000 packed PCM16 bytes, with complete 4 KiB canary bands. Hardware cache/coexistence
remains open. [Finding 83](../findings/83-sample-retirement-design.md) records the current-image
command-4 rejection audit, corrected DAG2 register display interpretation and exact eight-byte
ACK owner region. These observations require no core semantic change or regeneration.

The CF/DSP objects cross-build and instruction re-decoding passes. The initial focused unit
run passed 12 checks using actual assembled DSP and a prebuilt CF CPU harness. The new
TypeScript coupled acceptance test covers the complete payload, canary bands, duplicate and
stale-token handling, out-of-range rejection and partial checksum failure, with fresh boots
for sticky errors. It is prepared for parent review; no coupled transport or ten-second
playback completion is claimed here. The external-source report remains excluded pending
citation clarification.


Preflight launch blocker: patch sets are embedded by `packages/patchkit/src/devdata.rs`.
The existing prebuilt patchkit reports only `device`/`emulator` and refuses `ddr-proof`;
the prebuilt daemon likewise cannot discover the new set from the edited TOML. No daemon
build or coupled run was started. Parent decision is required between rebuilding binaries
with this set or using the existing explicit patched-image loading path. Focused unit
checks still pass (12 tests); lint/format checks and document pathcheck pass. Default
runtime test collection safely skips the four diagnostic scenarios without both explicitly
supplied preflight binary environment variables. Artifacts are retained under `out/runs/ddr-proof/`.


The parent resolved the embedded-set blocker by choosing the existing explicit-image path:
remove the proposed device patch-set addition, use the prebuilt CLI's
`apply --modules core/dsp-ddr-proof`, then load the patched image and stock base hash through
`firmware.load` on a stock `withSession` session. The test checks the returned load hash and
live injected/base identity against the exact plan, and retains stock identity, patched load,
plan and status artifacts. No daemon/native rebuild is needed. The explicit-module image
preflight succeeds (DN2 1.12, proof plus `core/expansion` only); the coupled run remains the
parent's next step.


### 2026-10-29 — transport passed; fixed resident playback preflight

The parent completed the isolated DN2 1.12 coupled transport: all 960,000 payload
bytes matched, both complete canary bands (8,192 bytes) matched, and duplicate DATA
was accepted without advancing the word count. Separate fresh sessions passed
out-of-range rejection, partial checksum failure and stale-token/watchdog cases.
Valid-run artifacts: `out/runs/ddr-proof/84d1a9b9-4bca-463e-8572-1ac1414c257d`;
logs: `out/runs/ddr-proof/full-transport.log` and `out/runs/ddr-proof/failure-cases.log`.
The valid source SHA-256 was `296a543415ca5c7e6e32a40f0692d18f6d8a46fe8841ba6c245e4720c4073162`,
patched image `bea2909e36322ad082809fdcd46e45d79b809e295b0baf26f48734bab9008e75`.
Reported 24.2-second valid-test wall time is not a transport throughput measurement.

The approved next stage places the SDK PCM16 consumer directly in the same core
adapter. A scoped diagnostic-only DN2 type 11 reservation
`core/dsp-ddr-proof:ddr_proof` leaves all existing allocations and dynamic range
16..23 unchanged. No default patch-set inclusion or public sample ABI is added.
A declaration in a framework module is supported by existing typed lowering.
The actual preflight closure is `core/dsp-voice`, `core/expansion`, `core/machines`,
`core/params`, `core/dsp-ddr-proof`; `core/dsp-link` remains excluded.

The six-word consumer admits READY at note-on, plays the fixed immutable 480,000-frame
view forward or reverse, preserves the machine-adapter return/stack contract, and
returns DONE at the exact end. Conservative instruction budgets are 2400/2600;
these are not measured hardware cycles. Seventeen focused transport/consumer tests
pass at preflight. The coupled valid test now repeats full transport with a windowed
200 Hz PCM16 triangle, resolves its actual registry binding and UI list position,
then captures at least 11.5 seconds with one held note. It checks early/late/end
cursor, exact completion, continued normal frames, finite PCM and early/late periodic
audible content. WAV, state snapshots, UI and binding/image/descriptor evidence are
retained. Stock output workspace reuse or FX tails are not confused with the unit
check that an extra reader call after DONE emits zeros and reads no source.

The playback coupled run remains pending parent execution. Hardware cache/coexistence,
resource retirement/replacement, sample Transfer UI and user selection APIs remain open.


### 2026-10-29 — first playback evidence and observer correction

The first coupled playback attempt retained artifacts at
`out/runs/ddr-proof/5982a581-d387-491f-9d5b-5be402f1b872` and log
`out/runs/ddr-proof/transport-playback.log`. All assertions through exact cursor
`[0,480000,480000,0,65536,1]`, DONE and 15,000 renders passed. The normal-frame observer
then failed: it remained 21,478 across 257 snapshots while rendering advanced. Independent
WAV extraction (`playback-audio-audit.json`) found 552,736 stereo frames, all finite,
peak 0.02065205, early RMS 0.009347532 and late RMS 0.009347522; both windows have
lag-240 correlation approximately 1 and lag-120 approximately -1. Thus the captured
known 200 Hz content satisfies the planned early/late audio checks, although the test
verdict failed before reaching those assertions.

The parent raw-byte audit resolves the observer fault: current DN2 instruction `0x1c9da8`
is Type3b unsigned SHORT16 (`l=1,x=0,w=0`); its display omitted the load width. The stock
bounds check therefore applies to the low command halfword. Current CF builder
`0x400274b4` stores command 3 at `0x80005e60`; the next halfword at `0x80005e62` is pitch.
The diagnostic's 32-bit observer incorrectly compared both halves with 3. Only the observer
now extracts bits 0..15. The exact complete-word-4 proof admission gate is unchanged.
A focused assembled regression covers nonzero-pitch command 3 and rejects a high-half
forged command 4. All 18 focused tests pass; the rebuilt DSP object re-decodes 501
instructions. No core, decoder or formatter edit/regeneration is included.

The TypeScript test now writes state/audio/result evidence before state/counter verdicts.
The strict normal-frame progression assertion remains for the measured workload; a static
one-hook-call-per-render guarantee is not asserted. Parent rerun remains pending at this
checkpoint. Direct TS lint/format and Python checks pass; the parent's broader routed
check retains the known baseline path references and pnpm launcher constraint, while its
direct host coverage passed after rerunning socket tests outside the sandbox restriction.


### Final emulator milestone validation (2026-10-29)

The parent rerun **passed** the single full transport-plus-playback scenario after the
observer-only correction. Artifacts are retained at
`out/runs/ddr-proof/11723539-7f66-4fce-b663-45cd9ea3d3da`; log
`out/runs/ddr-proof/transport-playback-fixed.log`. Patched-image SHA-256:
`e079d4fadaa8d26512b714d290c23bd9421c33f6c858c8cac6b60294d0f9e933`.
The exact 960,000-byte payload and both complete canary bands pass before triggering.
Actual machine type 11 dispatches one note; cursor ends at 480000, DONE=1 and render
count=15000. The corrected normal-command count advances 21798→39082. The tap reports
11.514 seconds and the WAV contains 552,992 stereo frames, all finite. Early/late RMS
is 0.009347532/0.009347522; both expected 200 Hz windows retain lag-240 correlation
approximately +1 and lag-120 approximately -1. All strict playback assertions pass.

Eighteen focused tests pass, including adapter ABI, unready/after-DONE zero-source-read
checks, forward/reverse end/retrigger behavior and the nonzero-pitch observer regression.
The parent also passed the three transport failure scenarios before playback integration;
the wire/protocol semantics are unchanged by the observer correction. Generation and
cross-build/re-decoding pass; scoped Python/TS lint/format, pathcheck and diff checks pass.
The routed changed check passes Python lint/format and layercheck but retains ten baseline
path-reference failures; its host/type phases were blocked by the existing pnpm 12 shared
node_modules launcher constraint. Direct host Vitest passed 187 tests with 16 skips;
the six socket tests blocked by sandbox `listen` EPERM all passed on the parent's permitted
retry. Logs: `out/runs/ddr-proof/changed-checks.log`, `host-checks.log`, `host-sockets-check.log`.
This is an emulator capacity/playback milestone, without hardware timing, coexistence,
resource replacement/retirement or Transfer UI/user-selection API claims. Parent owns
checkpoint, push and subsequent integration; no further coupled run is needed here.


### 2026-10-30 — LP0 0.4 implementation checkpoint (native pending)

The private machine now shares the actual LP0 CLOSE records, routes by latched
source track, pins each descriptor/PCM callback, and uses the tested SDK reader.
A publishes only after all pages, guards and terminal rearm; early CLOSE is sticky.
B has an independently owned guarded 16 KiB triangle bank at diagnostic rate 1/64.
DN2-only type 12 and the required machine/parameter/voice adapters are explicit
opt-in dependencies. No SDK/public ABI, emulator core or production allocator changed.

The existing native harness has an additional `DIGI_LP0_VOICES=1` case, with a
panel dispatch pilot and retained CLOSE snapshots, direct B block/WAV evidence,
and no reader-count injection. Preparing the exact 0.4 boot fixture, running
that case, independent object/image/artifact audit and publication remain the
parent's next steps. Historical 0.3 native evidence does not validate 0.4.


Owner checks for 0.4 pass: 90 LP0 tests, Ruff, scoped TypeScript format/lint/types,
changed-document pathcheck, object build/current/generation and whitespace checks.
The routed changed-files command is blocked at mise trusted-config symlink creation
by the checkout sandbox; the parent owns the routed check and merge gate.


### 2026-10-30 — callback observation boundary correction

The first actual-reader native attempt passed source dispatch and CLOSE/drain
checks, then failed an invalid assertion about retained stock output bytes.
The stock DN2 pipeline modifies those buffers after our callback returns.
Private per-track sequence-protected raw snapshots now capture the SDK output
and denied zero-fill before returning. The native test requires fresh A silence
relative to pre-CLOSE and fresh B raw PCM after drain; post-drain mixed spectrum
retains its separate capture origin. Failed v1 artifacts remain unchanged.
Parent preparation of a new exact-hash v2 fixture and bounded native acceptance
run remains the next step. This correction changes observation, not sampler
behavior, public ABI, production ownership or cache conclusions.


The corrected callback-snapshot implementation passes all 92 owner tests;
rebuilt CF/DSP objects contain 450/1,475 decoded instructions. The 2,500 render
instruction ceiling is conservative static budgeting, not a measured hardware
timing result. Independent snapshot source review is clean. Native v2 passed
fresh A-zero but the three-RPC B snapshot read timed out under continuous updates.
The test now reads sequence and PCM together with one synchronous DSP-worker peek,
retaining fresh/even sequence checks. No firmware change was required for that
second harness correction; v3 uses the exact verified v2 boot fixture.


### Checkpoint validation

Final native acceptance and its independent artifact audit pass. Routed
`mise run test:changed` passes Ruff, formatting of Python, path and layer checks;
its pnpm-launched host/lint/format/type phases fail on the existing linked
`node_modules/.pnpm` worktree layout. Direct installed-tool fallbacks pass:
222 host tests (16 skipped), scoped oxlint/Prettier and all five workspace
TypeScript checks. Logs are in `out/runs/lp0-voices-v3-run/`. Changed-document
pathcheck and whitespace checks pass. Dependencies were not modified. This is a
verified branch checkpoint, not a full merge gate or hardware acceptance.


### Cache-probe prerequisite checkpoint

The hidden nonfatal-allocator hypothesis is rejected by original bytes. DT2's
single capacity literal is not an adequate tail-reservation boundary. The selected
next bounded cache probe is an opt-in RR4 noncacheable DMA window configured by
the stock L1 helper before startup cache enable; RR1 was rejected because its
function is WBI/locking. RR4 ownership and silicon behavior remain separate limits.

Before implementing that hook, fix two concrete emulator observation gaps:
missing cache-range register names and byte-aligned inspection of compact MMRs.
Metadata plus an exact-word read API are implemented, with 12 passing focused
Python/native-library tests and independent source review. The generated-core
fingerprint remains `864c2c9906882085a7a388543d6a032f55a9490108c1222ce275c5457301323d`.
No instruction-core regeneration was needed; Cargo did recompile existing generated
code (about five minutes). The LP0 module remains at its verified 0.4 checkpoint.

- [x] Add register metadata; twelve focused Python/native-library checks pass.
- [x] Compile and test the exact-word DSP worker path; focused Rust test passes.
- [x] Build/freeze the daemon and pass live read-only DN2 1.12 RPC acceptance.
- [x] Repack image metadata and create a fresh cold state for range-register proof.
- [x] Implement the private RR4 startup window after resolving the preserved
  startup frame/DAG/SIMD context; fail closed on configuration failure.
- [x] Prove the configured window and repeat the actual per-track reader proof.

Live acceptance artifacts: `out/runs/cache-registers-proof/f9d605b0-13e7-4c90-8d4f-84d5c49f3b82/`.
Daemon SHA-256 `6008bfb0ecd3a99fe539b660e48b9d7c2eee1f93d5c6bfa3d55a772403930dac`.
CFG=`0x414141`, CFG2=`0xf0` remain unchanged, an address exceeding u32 is
rejected, and DSP frames advance from 2 to 39 without a reported fault. The run
held the exclusive shared-workspace lock and completed in 1.34 seconds after
waiting 35.57 seconds. This is a functional check, not a performance claim.

The first attempt exposed two test assumptions: RPC responses include evolving
session/timing fields, and legacy unnamed RR4 addresses return overlapping RAM
fallback bytes rather than null. Corrected comparisons isolate addr/value and
retain those RR4 reads only as explicitly untrusted diagnostics. The failed run
is retained at `out/runs/cache-registers-proof/19abfaea-9c76-47a2-be37-90f0af395de1/`.

Changed-file routing passes Python lint/format, pathcheck and layercheck. Its pnpm
host phases remain blocked by the existing linked worktree dependency layout;
direct installed-tool lint/format/type checks pass, and the related-host-test
selection contains no tests. No dependencies or generated instruction core were
changed. This checkpoint is not the full merge gate or hardware acceptance.

The DDR ownership investigation is bounded to an ownership table: stock owner,
authoritative allocation/configuration boundary, reset/bypass paths and positive
exclusion evidence. A profile-only declaration or absence of discovered writers
does not reserve DDR. Continue only to close a named boundary gap; do not pursue
an unbounded audit of arbitrary computed stores.


### 2026-10-30 — LP0 0.5 boot window and coupled-startup blocker

- [x] Implement the private 4,352-byte aligned RR4 window (descriptor/state/RX),
  guarded startup helper call and fail-closed receiver admission.
- [x] Pass all 101 owner tests, module build/redecode, generated/current-object
  checks, lint/format/types, and independent source/assembled-byte review.
- [x] Repack current cache-register metadata and execute the exact patched DSP
  from preinitialization to idle with bounded existing native tooling.
- [x] Fix coupled patched-DSP startup ordering, then repeat actual two-track proof.
- [x] Confirm restoring its cached ready pair preserves the initialized window.

Fresh fixture: `out/runs/lp0-cache-v5-fixture/provenance.json`. Patched SYX SHA-256
`161f374658148899461089d7d93dafe379e51c897bfd2abaf9151351022b3128`.
DSP reached idle `0xb88aa5` after 563,500,000 instructions (44.52 seconds).
Boot status=1/error=0; RR4 interval `[0x2008b3c0,0x2008c4c0)` and CFG2=`0x3f0`.
All four pre-exchange transfer counts remain `0x566`. This is initialization
and register evidence, not hardware coherence or production DDR ownership.

Full coupled run `out/runs/lp0-proof/c2d01474-8064-481d-9321-777da63eec61/`
failed at receiver setup: CF observed status=4/error=8 (boot setup missing),
so no transfer or playback acceptance is claimed. The daemon imports an already
initialized cold-idle state then reapplies the DSP diff, including zero-valued
initializers over the module's boot status and window pointers. Retained diagnosis:
`out/runs/lp0-cache-v5-fixture/double-patch-diagnosis.json`.

Selected correction: for changed DSP firmware, initialize from the base raw cold
state with the exact patch applied once, reach verified idle before ColdFire starts,
then attach without reapplying it. Preserve cancellation/halt/instruction/wall limits
and invalidate old ready-pair cache keys. Missing raw cold inputs must fail clearly,
not silently use an already initialized state. This follows the existing ready-tool
DSP-first order; no module admission bypass, manual state repair or core regeneration.

Startup wrapper refuses incoming SIMD before register saves, then guards M5/M6/M7,
M14 and L4/L5 while preserving circular stack L6/L7. The initial direct MODE1 store
alternative misencoded in the assembler; the final BIT TST/clear/SISD TF guard was
independently decoded and checked. Existing AudioTask-tail SIMD save handling is
outside this new boot seam and remains an explicit limitation to resolve separately.


### LP0 0.5 final acceptance and validation

The final run `b734a462-c9b0-4a67-a5a4-e0982c845d27` passes: 235 packets,
960,000 bytes exactly equal to the expected payload, both 4 KiB guards unchanged,
A reads fixed at 667 after drain while B advances 436→740, fresh A silence and
finite B waveform/tone. Cached restore reports `cache_hit=true` and
`start_used=snapshot-cache`, retaining RR4 bounds and CFG2=`0x3f0`.
The preceding run `18fb98a6-266c-4fc5-830b-641afca9bb2f` passed playback but its
new cache assertion loaded the already active image (a no-op); the final test
switches to stock before restoring the patched snapshot. All failed evidence is
retained. Final process wall time was 124.74 seconds, excluding lock wait; this
is a bounded functional run, not a performance comparison.

Frozen daemon SHA-256: `7cc6f7fc12cb7cc9fcdf2187a1907053b9556e1ce621877452696f5bb01f657f`.
DSP object SHA-256: `c272ea94f435abf8ea31f089d965fb584a0f25cef9865c4c752c05d5cf061084`.
WAV SHA-256: `9a69e4962b4c71dd0f08ce996f7e32db120f92dcfbcfc018c14b40efc1f2d06f`.

Validation: 101 owner tests; four cold-initialization Rust tests and one cache-key
test; native actual-reader/cache acceptance; generated/current-object checks;
222 host tests (16 skipped); all five workspace TypeScript projects and scoped
lint/format. Changed-file Python lint/format and layercheck pass. Pathcheck passed
after staging the new declaration so its references were discoverable. The pnpm
routing still cannot use this linked worktree dependency layout; direct installed
tools were used. Six socket-based host tests initially hit sandbox EPERM and pass
with socket access. No dependencies changed. The dual-core build and full merge
gate were not run; this is a verified branch checkpoint, not merge acceptance.


### DT2 integration follow-up (in progress)

- [x] Confirm both generated cores match the branch fingerprint and snapshot them
  under `out/runs/dt2-startup-validation/` to avoid concurrent regeneration races.
- [x] Prepare private current-metadata DT2 1.17 inputs and existing patched-pluck
  fixture objects; no object changes or instruction-core regeneration required.
- [x] Add an opt-in DT2 cold/cached startup regression, including DT2 core selection.
- [x] Complete dual-core compilation, focused Rust checks and native DT2 acceptance.
- [ ] Resolve DT2 before-reclaim/overwrite ordering for a shared-pool adapter.

The original-byte pool audit establishes conditional bounds, not an unconditional
reservation: initial rounded-size limit and append-end limit differ by 0x400;
subtracting the same delta preserves this difference. The initial comparison is
signed, and rounding/addition can wrap outside an established input domain. The
inspected record insertion/release/reset family preserves an in-bounds invariant
for nonnegative, nonwrapping sizes, but does not prove universal ownership.


DT2 lifetime follow-up: release entry precedes reference removal/reclamation;
header clear is too late. Reset needs close-all before its first header clear.
Stock slot counts are not DSP reader pins, and project reset destroys records
regardless of those counts. The next adapter design must keep CLOSE/DRAINED
progressing asynchronously while stock mutation is suspended; the current UI-tick
transport cannot safely be awaited by blocking that same UI path. No blocking
stock hook or unbalanced reference-count increment has been implemented.

DT2 integration run `out/runs/patched-dsp-startup/842ffcaf-b976-40ae-853b-5b41afdbb483/`
passes the bounded cold/cached startup regression with DT2 OS 1.17. The dual-core
daemon selects its DT2 backend (legacy generated label `dt2-1.16`; the loaded
firmware is independently identified by the 1.17 SYX hash). Four DSP startup
unit tests and the ready-pair key test also pass with `second-core` enabled.
Cold and cached states retain CFG `0x414141` and CFG2 `0xf0`; cached restore follows
an intervening stock restore and reports `cache_hit=true`, `snapshot-cache`.
The immutable daemon SHA is
`f2c167cc1c34b39ef2658277101af7287f90c1d406e0db0ee69fb16599ba810d`.
Input manifests, isolated generated-core copies and logs are under
`out/runs/dt2-startup-validation/`. This generic patched-pluck fixture checks DT2
startup/cache integration; the DN2 LP0 sentinel proof supplies the boot-variable
preservation regression. Neither is a hardware or production memory-owner proof.

The runtime service inventory confirms existing API 1.3 sample-bank loading and
API 1.4 reusable file listing already advance from the UI loop. Preserve those
contracts while introducing per-track binding; the private LP0 fixture is not a
shipping adapter. Next, inspect stock DT2 release/reset callers for a preceding
DSP stop/drain handshake before designing a new asynchronous continuation.

Validation: independent source/artifact review is clean. The new regression's
TypeScript project, four other routed TypeScript projects, scoped lint/format,
pathcheck and layercheck pass; related host selection finds no tests. The routed
`mise run test:changed` cannot run its pnpm-backed steps because this worktree's
linked dependency layout rejects `.pnpm` directory creation; equivalent installed
tools were used directly without dependency changes. The first native attempt
hit sandbox socket EPERM; the authorized local-socket rerun passed. No full merge
gate, hardware test, PCM pin change or instruction-core regeneration was performed.

### Current decision and next discovery — stereo shared project pool

The bounded stock header/reader audit is complete (finding 81). Header receipt
updates slot metadata; the immediate renderer uses a cached voice pointer. Neither
the inspected CF sender nor receiver supplies a proven old-reader drain grant.
This does not exclude wider stock coordination, but header clear is not a usable
shortcut around the pre-reclaim lifetime obligation.

The user requires full stereo on both devices, 16 tracks sharing a project sample
pool with no fixed per-track partitions, and deduplicated backing for references
to the same asset. Long samples must be constrained by actual capacity and format
limits, not the historical ten-second fixture. Stock DT2-like capacity is the
product target; parity has not been proved. Keep the shared engine and
runtime-shipped adapters, and stop only the selected track's old notes on selection.

The proposed 16 MiB tail recommendation and request to choose a stock-capacity
tradeoff are withdrawn. No reservation patch is approved. The allocator and
reader-lifetime audits remain evidence about obligations, not approval for a
smaller pool or proof of a safe shared-pool adapter.

Next discovery is bounded to:

- Inventory stereo representation, channel layout, sample-rate/encoding support,
  frame/length fields and long-sample arithmetic limits on both current images.
- Establish DT2 stereo pool accounting, including guards/alignment, actual usable
  capacity and deduplicated slot/reference ownership; distinguish the 400 MiB
  allocator observation from achieved stereo product capacity.
- Establish DN2 ownership, usable backing capacity, delivery and reset/coexistence
  bounds for the same shared project-pool contract, without fixed track partitions.
- Carry forward the asynchronous pre-reclaim suspension/resume and exact reader-drain
  obligations for shared DT2 backing across removal, replacement and project reset.

Return the concrete capacity/format/ownership gaps before proposing an allocation
change. The historical mono and ten-second proofs do not complete these requirements.

### Stereo implementation checkpoint

- [x] Inventory current SDK/host sample restrictions separately from firmware limits.
- [x] Add explicit stereo mode to the allocation-free native PCM16 stream parser;
  preserve the existing mono constructor and count capacity/completion in frames.
- [x] Check channel preservation, fragmented input, output backpressure, malformed
  alignment, truncation, trailer failures and long-header arithmetic: ten focused
  stream tests pass, with independent source review clean.
- [ ] Integrate stereo sample views and readers with the production pool/binding
  contract on both devices. Parser support alone does not establish stereo playback.

`NativePcm16Stream::new_stereo` preserves interleaved left/right PCM16 in packed
little-endian staging words. It does not expand whole samples to float32. Header
tests include a 400 MiB PCM payload without allocating that payload: this proves
parser arithmetic, not device capacity. The old mono `new` remains strict so an
existing mono consumer cannot silently accept and misinterpret stereo data.

Remaining implementation limits are explicit: the public pinned-bank decoder and
runtime source loader still accept only 8,192 mono float frames. The numerical
stereo reader added below does not yet replace that public loading contract. The host WAV converter already preserves stereo but defaults to 64 MiB
source and 128 MiB output PCM limits and holds whole-file conversion buffers.
These are software resource limits, not hardware capacity measurements. Large-file
host conversion needs a bounded-memory design before simply raising those defaults.
The native wrapper uses a 32-bit PCM byte length; the stream parser additionally
checks that header, payload and trailer length fit that representation.

DT2 original-byte stereo accounting is now recorded in finding 81: native files
are interleaved PCM16, stock DSP backing has two channel planes, allocation uses
rounded whole-file size, and the packet carries per-channel bytes which lookup
converts to frames. The new SDK stream's interleaved staging is not a description
of the stock pool layout. Next bounded firmware check: header PCM length versus
file size/allocation validation. Production capacity and lifecycle remain open.

Validation: focused SDK stream tests, independent source review, scoped rustfmt,
`mise run test:changed` (path/layer checks) and changed-document pathcheck pass.
No full merge gate, device run, firmware patch or generated-core change is claimed.

### Stereo numerical reader and current adapter boundaries

- [x] Implement a shared planar packed-PCM16 stereo block reader, composing the
  unchanged mono primitive from identical initial cursor state. Both channels
  share start/end, fractional pitch, reverse and completion; the cursor advances
  once. Numerical descriptors are not a new public sample-resource ABI.
- [x] Validate assembled stereo and existing mono tests: 142 pass, no skips;
  independent source review is clean, Python lint and changed checks pass.
- [ ] Extend machine output routing on both devices without silently duplicating
  mono output or bypassing stock processing. Current machine 1.1 has one 32-float
  output; DT2's adapter copies it to the paired voice, while DN2's hook feeds a
  mono per-slot processing chain. A second SDK pointer alone cannot solve DN2.
- [x] Verify current DN2 channel/rate transitions and the first actual stereo mix
  before selecting a backward-compatible machine contract extension.

The numerical stereo ceiling is 4,396 instructions including caller argument
loads and DK_CALL. Its ten local words plus the nested call reach entry I6−25
for writes and I6−26 for SP. These are instruction/stack bounds, not hardware
timing or proof of sixteen-track throughput. No instruction-core regeneration or
module object changes are required by the unintegrated primitive.

DT2 validation audit: the inspected loader compares bytes read with filesystem
size but publishes a separate native-header PCM length. Nonempty-pool allocation
and loading can use separately obtained metadata sizes. A safe adapter needs
canonical checked geometry and stable asset identity, not just loader success.
The potential DSP-side retirement shortcut also needs more work: stock LP0
completion invokes page/guard copying inline from an IRQ-reachable handler.
Invalidation alone cannot stop a reader already interrupted with a cached pointer.
Root is evaluating an explicit exclusion/deferred-write proof, not assuming
AudioTask serialization. Details are recorded in finding 81.

### Bounded exclusion experiment and stereo processing audit

- [x] Assemble a masked stereo-reader / overlap-writer experiment with a genuine
  count pin and explicit exact-zero drain before overwrite; independent review clean.
- [x] Enumerate all 3,486 arrival boundaries for each modeled page/guard event,
  grouped into 24 equivalent first-enabled delivery states. All twelve tests pass.
  The unlocked, deliberately unsafe invalidation-only control overwrites at count
  one and demonstrates old-left/new-right tearing; the safe control refuses it.
- [x] Restore an initially disabled mask exactly and keep the event pending until
  explicit enable; stale subsequent callbacks produce silence without PCM reads.
- [ ] Establish acceptable physical exclusion latency and complete writer coverage
  before selecting whole-block masking as a production adapter policy.

The modeled reader executes 3,485 instructions with a largest masked span of
3,462, including wrapper work. This is one serialized maskable-event model with
ideal context preservation, represented overlapping spans and architectural stack/
bank models. It is not physical IRQ-pipeline, NMI, DMA, cache or hardware-timing
evidence. The model preserves the existing LOCK/UNLOCK NOPs and does not alter
instruction-core semantics. Metrics: `out/runs/stereo-exclusion.xml`.

[Finding 96](../findings/96-stereo-machine-processing.md) records a current-image
correction: DN2's downstream 64-float layout is two 32-sample channel planes,
not 96 kHz temporal upsampling. Its source seam still feeds a mono chain. Two
audited stages have independent recursive histories and read-only coefficients,
so stereo can share coefficient/control values without sharing audio history.
The finite ordered chain and all ten local state footprints are now audited;
external writers, route domains and lifecycle integration remain open. No machine
contract extension or late insertion bypassing stock processing is implemented.


### 2026-10-30 rebase and stereo replay checkpoint

- [x] Rebase the sampler branch onto `origin/main` at `d6b5d4bb`, preserving the
  pre-rebase history at `backup/dsp-sampler-pre-d6b5d4bb` (`813772e8`). Regenerate
  our core objects with the current assembler. Independent review confirms only
  toolchain/source hashes changed, with identical payloads and relocations.
- [x] Revalidate 154 assembled mono/stereo/exclusion tests, ten stream SDK tests,
  four coupled-startup tests and one ready-pair-key test. No core regeneration.
- [x] Pass DT2 generic patched startup/cache and DN2 selected actual-two-track
  reader/close tests with the rebuilt dual-core daemon. Independent artifact
  review confirms the bounded claims; this is not hardware or production stereo.
- [x] Audit all ten DN2 processing families' local state footprints. Record shared
  control updates, channel audio histories, scratch and the separate-ring gap.
- [x] Implement and independently review an assembled three-stage shadow replay
  proof using actual original code loaded at runtime. Six focused tests pass with
  unequal channels, changing controls, explicit reset and corruption controls.
- [ ] Prove actual route/live-in coverage, natural reset/retrigger/steal mirroring,
  snapshot stability, scratch lifetime, independent ring backing and pan/send
  integration before promoting the replay mechanism into the runtime adapter.
- [ ] Complete safe project-pool ownership and replacement/reset fences on both
  devices, then connect shared stereo views, per-track binding and the picker.

Rebase validation artifacts: `out/runs/sampler-main-rebase/`; immutable daemon
`out/runs/sampler-main-rebase/stable/digi-emud-6bd36a197f14`.
DT2 run: `out/runs/patched-dsp-startup/5d429bbb-4b2c-4104-bd02-aefae54c2974/`.
DN2 run: `out/runs/lp0-proof/d2bed357-2973-4c48-9213-89bfe2bc2fca/`.
The first DT2 attempt stopped during stale test-fixture preflight; rebuilding
our cached patched-pluck fixture resolved it. DN2 selects one actual-reader case;
four other cases are intentionally not run. The new harness owner checks report
12 passed, two skipped because `SHARC_NATIVE_LIB` was not supplied; all six new
replay tests execute. No full merge gate or hardware acceptance is claimed.

The shadow replay is a synthetic subset, not a verified stock preset. Shared
controls are snapshotted before the left pass; right mutations remain live within
the replay, while independent right audio histories survive between blocks.
All replay tests use the default core, with the measured RECIPS and RSQRTS seed
tables (finding 102). See finding 96 for scope and evidence.

### Reciprocal emulator follow-up

- [x] Identify the current gap: ordinary RECIPS numeric seed, rather than a broad
  missing instruction/flag implementation. Public PRM 19-17 describes a ROM
  indexed by seven mantissa bits, but its table contents are not supplied.
- [x] Measure the seed tables on a Digitakt II and put them in the core
  ([finding 102](../findings/102-recips-rsqrts-seed-tables.md)); the approximation
  option is removed. The replay proofs run on the exact core.


Checkpoint preflight: Ruff, layercheck, 281 routed Python tooling tests and changed
reference checks pass. `test:changed` initially reports a new fixture as untracked;
staging it resolves pathcheck. Its pnpm host/format/type launches are blocked by
the shared worktree's linked `.pnpm` directory, so the installed executables were
run directly without dependency changes. Direct host checks total 222 passed and
16 skipped across three firmware-dependent files; six local-socket tests initially
hit sandbox EPERM and pass when that file alone is rerun with socket permission.
All five workspace TypeScript checks and changed-JSON Prettier checks pass. Logs
are in `out/runs/sampler-main-rebase/host-final.log` and `host-socket-final.log`.
The wrapper itself did not return success; these are the resolved constituent
checks, not a claimed full merge gate.


Reset follow-up: an original-byte audit resolves the `0x1c96aa` reset argument
through delayed-call setup to `0x2533e0 + R10`, matching the U/Y processing-record
base and `0x20` stride. This closes one pointer question; slot ordinal, branch
event semantics and complete reset/steal coverage remain open. Finding 96 records
the evidence and rejects an intermediate database-only pointer interpretation.


### Continued stereo integration discovery

- [x] Extend assembled replay to isolated `0xb82342`, `0xb81a16`, `0xb81526`
  leaves, with unequal audio, changing coefficients, private histories and
  meaningful corruption controls. Twelve total replay tests pass without skips;
  source review is clean. New leaves need explicit byte-pointer/32-bit NW context
  and run on the default core.
- [ ] Cover remaining control/processing families and case-5 scratch/blend, then
  prove the real ordered route and pan/send integration rather than treating leaf
  equivalence as a production stereo adapter.
- [ ] Resolve bounded reset conditions and pair first-order/U-Y reset events with
  shadow histories; retain explicit gaps for upstream dispatch and natural events.
- [ ] Assess a complete diagnostic CPU/DMA access guard for candidate pool ranges,
  including native and Wasm fast-tier bypasses. This is a feasibility investigation,
  not a selected production reservation or a quiet-run ownership proof.


Reset progress: the same conditional stock sequence resets both U/Y histories and
first-order words 24..27. It executes when a byte and a control-record word are
both nonzero. The local sixteen-record stride is identified; upstream indirect
callee preservation and event meanings remain open. Follow the two producers
before calling these note-on/retrigger/steal hooks (finding 96).


- [x] Add bounded replay tests for held-sample timing/history and shared block
  preparation: eighteen total replay cases pass, including table-read bounds and
  negative controls. Eight of ten local families have numerical replay coverage.
- [x] Trace reset word-5 producer to a packed frame byte and identify a clear writer
  for the other guard. Its nonzero producer and event meanings remain open.

### Candidate DDR diagnostic: implementation direction

Payoff: expose concrete stock CPU/DMA use of candidate project-pool intervals,
including same-value writes and reads, instead of relying on sampled word changes.
Confidence is high in the bounded host-layer mechanism, moderate in complete
integration until bypass tests pass. This may take a day or more including native
validation; stop expansion if missing paths require instruction-core changes or
if the instrument cannot distinguish its own bookkeeping sufficiently to interpret.
No production reservation or capacity is selected by this diagnostic.

The inspected raw fast paths share Mem page pointers and window caches. A candidate
DDR-only monitor can withhold watched-page fast pointers, invalidate cached windows
through layout_gen, and let normal execution fall back to observed accessors. This
must cover native CL and Wasm, cached windows, new-page allocation and reset; a
window refusal is not itself a data access. Root's review found additional required
rules: disable idle-loop skipping, capture synthesized/uninitialized read attempts,
and distinguish guest attempts from journal/rollback/host setup traffic. A plain
Mem-only counter is not a complete guest-access monitor.

- [x] Implement an opt-in diagnostic with an explicit event/lifecycle contract,
  tested first at the memory and semantic boundaries; no public module ABI change.
- [ ] Prove raw fast paths fall back correctly, imported/reset state reapplies policy,
  same-value writes and read-only traffic are seen, and diagnostics-off preserves
  behavior. Rebuild native and Wasm as needed; Python core fingerprint need not change.
- [ ] Only then run bounded stock coexistence scenarios over candidate intervals.
  A quiet run is bounded observation, never universal ownership or hardware proof.


### DDR diagnostic foundation checkpoint

- [x] Implement the opt-in Rust first-attempt diagnostic and explicit CPU versus
  storage attribution, including synthesized reads and symbolic-value write
  attempts with concrete addresses. Range changes invalidate raw pointer caches;
  reset preserves policy and clears the latch; monitoring disables idle skipping.
- [x] Pass eight focused monitor tests and all 21 runtime-crate units (overlapping
  counts), with independent source review clean. Shared-IR interpreter fallback
  is exercised; there is no full compiled-backend dispatch claim yet.
- [x] Resolve the existing Cranelift flat-access harness panic in a separate scoped
  change, then run the monitor proof through the actual compiled backend.
- [x] Expose and validate a bounded native diagnostic entry point for stock runs.
- [x] Validate real-lowering Wasm guard exits using the checked target-layout fixture.
- [ ] Validate full browser-engine fallback and monitor lifecycle integration.

[Finding 97](../findings/97-ddr-access-diagnostic.md) records the exact contract,
reset/import behavior and remaining coverage. No instruction-core regeneration,
firmware change, production allocation or hardware test is part of this checkpoint.


Native backend follow-up passes: three flat-access tests run reference and actual
Cranelift kernels, two focused guard regressions pass, and eight diagnostic tests
remain green. Independent review is clean. Reproduction and exact commands are in
`out/runs/ddr-monitor-native/`; finding 97 records the small failure-block ordering
fix. Next is an ordered emulator diagnostic control route and bounded stock runs;
this is separate from public module ABI and sample allocation.


### Parallel gain proof checkpoint

- [x] Exercise the final gain stage on two nontrivial trajectories, preserving
  one authoritative control evolution and replaying the same initial control
  state on unequal right audio. Actual entry reinitialization and advanced-control
  corruption are tested; all 22 replay cases pass without skips or reciprocal
  approximation in the new cases. Independent review is clean.
- [x] Prove independent backing for the remaining ring-backed numerical stage.
- [x] Prove a selected ordered route with scratch blending and output splitting.
- [ ] Complete production routes and pan/send integration.

The gain proof was developed in `/private/tmp/digikit-sampler-gain-proof`, on its isolated gain-proof branch, based on checkpoint `3e236b89`, while the native compiler
fix proceeded in the primary worktree. The coordinator applied and byte-compared
its two tested files; no secondary-worktree commit or independent publication was
made. The primary branch remains the integration and publication branch.


### Ring and source-continuation checkpoint

- [x] Prove independent ring backing with the actual constructor, wraparound,
  bounded accesses and deliberately shared-backing corruption detection.
- [x] Audit and replay the separate source continuation with explicit SIMD scratch
  context and five controls; no new instruction-core gap or approximation needed.
- [x] Independently review and integrate both additions; all 29 named replay tests
  pass without skips in the primary worktree.
- [ ] Integrate an ordered route including inline scratch blending and pan/send.

Finding 96 records the bounded domains and remaining lifecycle limitations.
The daemon DDR diagnostic route is implemented and source-reviewed; dual-core
unit checks and both native acceptance cases pass. The explicit helper access is
captured, status does not self-latch, invalid commands preserve state, and bounded
stock execution completes without faults. Finding 97 records immutable binary
identity and observation limits; no production memory ownership is established.

- [x] Expose and validate the ordered diagnostic RPC on DT2 1.17 and DN2 1.12.
- [x] Exercise the Wasm fast-path refusal with a focused V8 proof.
- [ ] Expand stock coexistence scenarios before selecting any production backing.
- [x] Review and integrate the composed source/filter/blend/gain/output-split
  proof from the secondary worktree; preserve its synthetic-route boundary.


### Composed route and Wasm diagnostic follow-up

- [x] Independently review and integrate a selected source/filter/scratch-blend/
  gain/output-split route, with four blocks and six positive/corruption cases.
  It compares both private pairs against isolated mono references. Its candidate
  left-A/right-B mapping is not a finalized pan policy or production ABI.
- [x] Audit the stock DT2 paired-input pan fragment: independent scaling agrees
  with the candidate diagonal mapping at this boundary; UI/full-pipeline equivalence
  is not claimed.
- [ ] Complete original engine context, shared cleanup, full routing, reset and
  send integration before promoting the replay into a runtime adapter.

The focused Wasm diagnostic proof exports six kernels from real lowering with a
simulated wasm32 pointer layout, then executes emitted Wasm under V8. Forty-nine
cases and four native flat-access tests pass; independent review is clean.
Full browser-engine fallback and RPC coverage remain separate from backend exits.

The integrated ordered replay suite passes all 35 tests without skips. No core
semantics, generated firmware material or production sample allocation changed.


Next bounded integration evidence: complete the local DN2 reset/initialization
helper map for the private histories. Mirroring actual helper executions avoids
inventing note-on/retrigger meanings for partially traced control flags. Complete
external writer/lifetime coverage remains a separate obligation. The shared ABI
will preserve the existing mono input prefix; no public extension is implemented
before original routing, cleanup and reset boundaries are ready.

### Reset and execution-context audit checkpoint

- [x] Map the four remaining local reset families and their extra engine call
  sites. Preserve the b82342 snapshots 48..50; reset is not generic record clearing.
- [x] Check immediate case-1 preparation for audio-history feedback: no such reads
  in the inspected setter chain; global consumers remain outside the result.
- [x] Trace the immediate AudioTask/engine caller chain. It supplies no local
  interrupt-exclusion proof; do not assume snapshot stability from the task name.
- [x] Add bounded actual-helper reset replay and corruption controls, independently
  review and integrate. Missed resets alter audio; blanket snapshot clearing is
  detected at the reset boundary.
- [ ] Resolve the production capture/commit boundary and relevant asynchronous
  writers before extending the public stereo contract or shipping a route replay.

Finding 96 retains exact addresses and limits. The last item is an architecture
constraint, not evidence that an IRQ has actually modified these records.

Integrated reset checkpoint: all 44 named stereo replay cases pass without skips.
The reciprocal follow-up is read-only: determine whether public documentation
specifies exact seed recovery or a hardware measurement is required. No
instruction-core changes or regeneration are authorized by this follow-up.

The inspected public manuals specify ROM indexing and exponent/sign behavior, but
not the seed values ([finding 98](../findings/98-reciprocal-seed-fidelity.md)). The
values come from the device measurement in finding 102.

### Completed private native prototype: first processing stage

The opt-in DN2 1.12 callsite wrapper now passes a bounded native identity check.
This establishes an exercised caller context and reset, alongside the model
fixtures. Stop expansion if caller context, state ownership or dispatch cannot
be bounded; this checkpoint does not select a production ABI or backing pool.

- [x] Add an opt-in DN2 1.12 proof adapter for the first-order stage and its local
  reset callsite. Keep the original left call and output authoritative.
- [x] Use linker-owned private L2 records/audio for sixteen slots (roughly 4 KiB),
  avoiding any new DDR reservation. Copy input for an identity right-channel proof;
  capture controls before left, preserve independent right histories and mirror
  the actual reset helper. Do not install this in default runtime builds.
- [x] Expose bounded diagnostic counters/context through existing proof mechanisms;
  require right/left identity, unchanged original state/output and preserved call
  context. Compare the original left output with an unmodified baseline.
- [x] Verify fault-detection controls and the owner checks before considering more
  stages. No public stereo SDK ABI, external ring allocation, full-route claim,
  hardware access or performance claim is part of this first prototype.

Known callback bindings now identify SPI ring-selector updates and SPORT task
notification. No concrete processing-setter path was found from those callbacks.
The generic LP0 driver's accepted range is not evidence of an actual normal
processing-record writer: no stock submit producer was identified in the bounded
layer, while the private LP0 proof submits only its own fixed L2 receive buffer.
Mutable callback registration, computed dispatch and complete writer exclusion
remain qualified. The experiment preserves existing interrupt policy rather than
inventing a whole-route exclusion guarantee from these observations.

**[D] Native checkpoint:** `core/dsp-stereo-proof` retargets only the engine's
first-order render/reset calls. Its private L2 data is 3,992 bytes. The 23 model
cases pass; the opt-in native acceptance also passes with 2,400 left/shadow calls,
one reset mirror, zero mismatches/fallbacks, 2,400 nonidentity-coefficient blocks
and 76,799 nonzero input samples. The 100 ms stock/patched captures contain 4,800
stereo frames each, identical whole-WAV hashes and zero alignment shift. Both
snapshot loads and audio activation are asserted, with forced reload and hashed
ready inputs. Final regression TypeScript and scoped oxlint checks pass.

Evidence and exact commands are retained in ignored
`out/runs/dsp-stereo-proof/commands.json`,
`out/runs/dsp-stereo-proof/native-acceptance.log` and
`out/runs/dsp-stereo-proof/native/7388abd1-c4a3-4fdf-8199-edc11d2bfc16/result.json`.
The legacy core profile label `dn2-1.11` is not a firmware-version claim; the CF
version, original base hash and ready-input hashes establish this DN2 1.12 run.
The observed minimum entry frame minus the model-exercised 172-byte store depth
leaves 3,132 bytes within the corrected 4,000-byte AudioTask allocation. This is
scenario-specific arithmetic, not a native store watermark or IRQ/scheduler bound.
See [the stage evidence](../findings/96-stereo-machine-processing.md) and
[the stack correction](../findings/83-sample-retirement-design.md).

Full-route integration, coherent capture/commit, cleanup, writer exclusion,
production backing ownership, public SDK ABI, performance and hardware acceptance
remain open. No production memory reservation or default activation was selected.


**[D] DT2 pre-store follow-up:** original 1.17 bytes establish calculable page
and ten-byte guard spans before the event-4 writes, but the page path leaves old
slot metadata readable and the later publisher supplies neither generation nor
READY-last. This is an interception seam, not successor identity, completion or
admission. See [finding 81](../findings/81-stock-sampler-dependencies.md); no
production memory policy follows from it.

### Private first-stage polarity isolation follow-up

- [x] Add a pre-first-use opt-in polarity mode, leaving identity as the default;
  invert only copied right input and initial live histories, preserving stock left.
- [x] Compare inverse right audio/live histories with signed-zero normalization,
  reject nonfinite trajectories and expose bounded relation/guard counters.
- [x] Add copied-left input/history negative controls and context/state assertions;
  extend native acceptance to both modes from the same forced snapshot.
- [x] Build/check/decode the object and run scoped model/type/lint/format checks.
  Both native modes pass under coordinator-owned execution. This is a finite, same-mode metamorphic
  independence test, not an independent numerical oracle, 40-bit proof, production
  output route, public ABI or memory/interrupt policy.


The polarity implementation passes 39 model cases, object build/conformance and
677-instruction re-decode, gen-check, regression TypeScript and scoped lint/format
checks. Private data is 4,020 bytes; the observed model identity-fixture maximum is
4,315 instructions and the exercised store depth remains 172 bytes. Mode is
latched by the first accepted render/reset; invalid or changed mode refuses future
shadows until reload. Polarity mode rejects nonfinite inputs/coefficients/live
histories and outputs with sticky refusal; identity retains its earlier exceptional
comparison behavior. Both inverse audio and live histories are checked, treating
signed zeros as equal.

- [x] Run both native modes from the same forced snapshot with the immutable daemon.
  Each mode records 2,400 render/shadow calls, one mirrored reset and zero mode
  errors, nonfinite observations, mismatches or fallbacks. Both 100 ms captures
  retain the stock whole-WAV hash, 4,800 stereo frames and zero alignment shift.
  Polarity mode records 76,799 nonzero opposite-sign audio relations and 4,800
  opposite-sign live-history relations; captured numeric MODE1 is `0x39011cf8`.
  This is finite, same-mode metamorphic independence evidence, not an independent
  numerical correctness oracle or whole-route acceptance. Details and result
  paths are in [finding 96](../findings/96-stereo-machine-processing.md).

The bounded U/Y caller/context audit and positive early/reset interpreter
profile are complete; the next checklist records the implementation boundary.
The coordinator approved the bounded private U/Y wrapper implementation below;
no new production memory policy or output route follows from it. Full-route lifecycle, pool ownership and hardware
acceptance remain open.

**[D] DT2 direct-loader follow-up:** the general loader uses direct page/header
sender `0x40153a10 → 0x400cd79c`, not queued-preview `0x40153f34`. Its per-packet
mutex/wait does not supply a DSP-applied tuple or prove whole-load serialization.
See [finding 81](../findings/81-stock-sampler-dependencies.md); successor identity,
geometry validation and ownership/admission remain open.


### Bounded U/Y integration next slice

- [x] Audit both engine render callsites and local reset, including delayed argument
  setup, shared record/history and continuation live-outs. Record positive early
  and reset interpreter coverage without treating absent late/fast rows as dead code.
- [x] Review the wrapper approach before implementation: cover BOTH early
  `0x1c959f` and late `0x1c9b7a` calls to `0xb81368`, plus reset
  `0x1c96aa → 0xb8135a`. Keep the other five helper callers unchanged.
- [x] Preserve one right history per slot for shared record `0x2533e0+0x20*v`;
  capture input and coefficient words 2..5 before every occurrence, retain live
  histories 6/7 between placements, and mirror the actual two-word reset. Do not
  infer stable exclusive placement from the G table or reinitialize between sites.
- [x] Add bounded caller/context and model checks for both placements, sequential
  placements sharing a slot, reset, left authority, identity/polarity controls and
  fallbacks. Preserve original call/frame/delay semantics and scalar/SIMD guards.
- [x] Run focused native acceptance only after review, retaining unchanged audible
  stereo baseline and reporting which placements/reset actually execute. Existing
  mode-0 interpreter profiling proves early/reset coverage only. This is not a
  full-route, SDK ABI, ownership, IRQ-policy, hardware or performance decision.

See [the audit and profile](../findings/96-stereo-machine-processing.md). The
existing bounded interpreter run passes with 2,400 early visits, one reset and
2,850 total helper-entry visits in each stock/patched profile. Late remains
unexercised in that scenario, and gate stability remains unproved.


Implementation checkpoint: U/Y is a separate appended module-local source in the
same opt-in object. It captures every occurrence into one persistent right record
per slot, with no per-site trampoline or claim of placement attribution. It shares
the pre-first-use mode knob and global mode-error refusal; U/Y finite refusal is
strict in both modes, while existing first-stage exceptional behavior is unchanged.
The first-stage source is unchanged and the combined model asserts an identical
linked instruction prefix at fixed placement/bindings.

Both named model files pass 75 cases (39 first-stage, 36 U/Y), including shared-slot
repeated modeled early/late occurrences, changing controls, reset/retrigger,
finite-refusal, input/history corruption and cross-stage mode-switch controls.
Packaging preserves all original delay instructions, including computed U/Y reset
setup, and changes only the five selected calls. Combined private data is 6,760
bytes and code 5,830 bytes/1,375 re-decoded instructions. The largest observed U/Y
fixture is 3,038 instructions; its exercised store depth is 176 bytes (first-stage
172), so native headroom arithmetic now uses the maximum 176. These are model
fixture observations, not hardware performance or universal stack bounds.
Build/conformance, gen-check, types, lint/format, layercheck and pathcheck pass.
The native fixture now gates both stages in both modes, including U/Y reset and
opposite-sign relations. The combined two-mode native acceptance now passes.
Each stage records 2,400 render/shadow calls and one reset mirror per mode, with
zero mode errors, nonfinite observations, mismatches or fallbacks. In mode 1 each
stage records 76,799 opposite audio and 4,800 opposite history relations. All stock
and patched WAVs retain hash `6be721f8b1649c0a037bd8bd1a0998ff5847ed80de31b1907f98b01604555b0c`,
4,800 stereo frames/100 ms and zero shift. The minimum observed entry minus the
176-byte model-exercised store depth leaves 3,128 bytes in this scenario; this is
not a native store watermark or IRQ/scheduler bound. This Pre-only run left late placement unexercised; the later held-note route
profile below supplies positive coverage. Shared hook counters do not distinguish
the sites. See
[finding 96](../findings/96-stereo-machine-processing.md) for exact result paths.


**[D] DT2 reference-tree follow-up:** the audited family stores unique slot IDs;
rebuild uses node keys as unchecked slot-array indices, duplicate insertion adds
no reference, release still clears the slot/header, and record moves relocate
the tree/sentinel. It supplies no balanced slot-independent pin. See
[finding 81](../findings/81-stock-sampler-dependencies.md); do not substitute a
fake key or raw record pointer for a supported backing lifetime contract.


### Private per-stereo-frame fetch experiment checkpoint

The coordinator approved this private fixture/model experiment. It is not shipping
memory/interrupt policy. The purpose is to reduce the current whole-block masked
read scope while retaining coherent frame data and generation retirement.

- [x] Review a private fetch boundary per stereo frame: cached-generation admission,
  both channels' packed interpolation taps and all required scalar copies occur
  under the mask; release the backing count before computing from private copies.
- [x] After denial, zero the remaining frames without relatching an epoch. Prove
  that no later computation dereferences backing after releasing its count.
- [x] Keep callback cursor/scratch storage owned until that callback is finished,
  even after its backing count reaches zero; backing retirement alone must not
  authorize reuse of callback state.
- [x] Test actual DDR NW32 aliasing: byte address is
  `0x80000000 + 4*(w-0x10000000)`, not `4*w`. The new private fixture uses
  actual aliases for page/guard stores and packed reads; prior stand-in spans
  remain numerical only. No production ownership policy follows from this proof.


The initial reader implementation paused for a pinned-tool store-width probe:
both `DM(0, I4) = R0 (SW)` and `DM(I4, M6) = R0 (SW)` are
accepted but emit ordinary four-byte stores at byte address `0x80001002` in the
model. The existing assembler postprocessor handles absolute short-word loads,
not these stores. The subsequently approved original DT2 guard helper supplies
the exact ten-byte
write; the unsupported wrapper source form remains a separate follow-up. This is
not evidence of an emulator semantics bug; no width approximation,
assembler/core change or production policy was introduced. Probe evidence remains
under ignored `out/runs/stereo-frame-fetch/store-probe.json`.


The private fixture now passes 58 owner model tests with no skips. Actual stock
page geometry and five original-helper halfword guard stores affect the same DDR
aliases read by both channels, including an odd terminal word's unused half.
Every arrival boundary is enumerated for six representative page/guard/plane
configurations (31,134 arrivals), reduced by explicit commutation to 33 fetched-frame
classes each. Resumed and stale callbacks have no forbidden reads; an unsafe
split-channel control tears a real frame. Maximum enabled-entry masked reader
spans are 80/81 model instructions, totals 5,172/5,204; the existing whole-block
reference measures 3,462/3,485. These are different-body instruction observations,
not cycles, performance or IRQ latency. Incoming-disabled mode remains disabled.
See [finding 99](../findings/99-stereo-frame-fetch-exclusion.md) for the ownership
assumption, exact helper provenance, negative controls and remaining production
gaps. Existing SDK kernels/public ABI/runtime/core and current exclusion tests
are unchanged.

- [ ] Separately resolve unsupported `(SW)` store source-form handling; no assembler
  or core change is part of this proof, and byte-store forms remain unprobed.
- [ ] Establish production callback-private cursor/scratch lifetime independently
  of backing count/drain before considering runtime integration.


### Scoped raw packed-fetch extraction

- [x] Approve only an eight-instruction caller-owned raw containing-word macro;
  keep scalar/linear/address-domain/protection preconditions explicit and add no
  validation, state, locking, unpacking, interpolation or resource/output ABI.
- [x] Refactor only the private frame fixture's two fetch pairs, retaining order,
  private copies and one admitted stereo protection scope; rerun its 58 checks.
- [x] Document the reusable SDK primitive and worked private proof, while leaving
  old PCM kernels and production exclusion/lifetime policy unchanged.


The SDK now exposes `DK_SAMPLE_PCM16_FETCH_WORDS` in
`sdk/dsp/sample_pcm16_fetch.inc`. Its eight raw-fetch instructions preserve input
base/indices and require caller-validated nonnegative bounds, nonwrapping full
containing words in one address space, SISD/PEYEN clear, a linear caller-owned DAG
and protection across both reads. It supplies no validation, state, conversion,
masking, atomicity, lifetime or cache policy. Only the private frame fixture uses
it here; old kernels and production policy remain unchanged. The original 58
functional/interleaving controls retain the same 80/81 masked and 5,172/5,204 total
instruction observations. A new shared-index-register/last-DDR-word case covers
the macro's broader caller operand domain. See the
[SDK contract](../../sdk/dsp/README.md) and
[finding 99](../findings/99-stereo-frame-fetch-exclusion.md).

### Native BW Routing late-placement acceptance extension

- [x] Calibrate the legitimate DN2 1.12 panel path with a bounded stock probe:
  FLTR page2/2, ENC G (BW.RT), observed Pre→Post readback and G byte1. No direct
  parameter/control-memory writes; retain exact probe inputs and UI evidence.
- [x] Add a held-note route-switch variant in both modes, preserving the original
  Pre scenarios. Compare identical instruction-scheduled stock/patched panel
  changes from forced snapshots, retaining early history and both-stage gates.
- [x] Type/lint/format and record commands/provenance. Coordinator owns final
  acceptance and interpreter callsite profile; aggregate counters or gate bytes
  alone do not prove late-site execution. No DSP/SDK/ABI or production policy edit.


All four native cases pass. The route variants retain nonzero early history, then
use only scheduled FLTR/ENC G panel events during the same held note. Each stage
records 19,200 render/shadow calls and one reset mirror per mode, with zero errors;
the reset count remains one across the route switch. Mode 1 records 614,399
opposite audio and 38,400 opposite history relations at each stage. Both 800 ms,
38,400-frame stereo captures have identical stock/patched whole-WAV hashes and
zero shift. The current combined model-depth arithmetic remains 176 bytes below
the minimum observed entry, leaving 3,128 bytes; it is not a native watermark.

The filtered interpreter route case also passes, with one case executed and three
intentionally filtered. Paired stock/patched profiles each show early `0x1c959f`
10,160, late `0x1c9b7a` 9,040 and reset `0x1c96aa` one, at MODE1 `0x39011cf8`.
Both status records have zero AOT/fast/block execution and zero traps. This is
positive native placement coverage for the calibrated held-note scenario, not
proof of global gate stability, every lifecycle or production route integration.
Exact commands and results are under `route_extension` in ignored
`out/runs/dsp-stereo-proof/commands.json`; [finding 96](../findings/96-stereo-machine-processing.md)
records the page/encoder calibration and profile identities.

**[D/O] Callback lifetime next step:** the audited uninterrupted AudioTask call
chain serializes only its own calls, and adapter output use continues after module
return. Proposal: keep cursor/scratch boot-pinned and untouched by backing
replacement; audit selection/row state reuse and the existing callback boundary
before binding install/reset. Non-reentrancy remains open; tracing is optional
and cannot prove safety from absence. See [finding 99](../findings/99-stereo-frame-fetch-exclusion.md).

**[D/O] Boot-load exception:** failed row invalidation can clear publication flags,
then unreserve DSP/free CF storage, statically permitting erroneous reuse. Normal
boot pinning is insufficient until this cleanup path is fixed; actual overlap is
not observed and ACK/block-tail ordering remains under audit. See
[finding 99](../findings/99-stereo-frame-fetch-exclusion.md).

### Failed DSP-load cleanup retention fix

- [x] Make row invalidation report failure without discarding publication markers;
  stop after the first failed write and separate invalidation from unreserve.
- [x] Require confirmed DSP invalidation after CF admission/drain and before fini,
  registry removal or DSP/CF free; uncertain cleanup retains ownership/storage.
- [x] Stop the load queue in a distinct sticky retained phase on cleanup failure,
  report the original error once and freeze shared transfer sources until reboot.
- [x] Exercise actual runtime branches through bounded cfg(test)-only host boundary
  injection, including refusal/error/no ACK, success and prepublication failure.
  Unit state-machine evidence does not establish physical DSP callback quiescence.

The ten actual-runtime host cleanup tests pass; the fresh ColdFire runtime object
re-decodes and is source-current. Dedicated unchanged pluck artifacts for DT2/DN2
pass coordinator-owned normal-loader acceptance on DT2 1.17 and DN2 1.12.
Both upload/persist/select/reboot and DSP publication/no-fault cases pass; this is
not rendered-audio correctness or native failure-injection coverage. The cfg(test) seam
mocks hardware/link and CF pointer boundaries only; physical DSP quiescence,
ACK/block-tail ordering and callback non-reentrancy remain open. Exact commands
and artifacts: `out/runs/dsp-cleanup-retention/commands.json`.

### Source-format inventory before the next integration slice

**[D/O]** [WAV conversion](../../packages/dksample/src/wav.rs) accepts one/two
channels and produces 48 kHz interleaved PCM16; the
[canonical native file](../../packages/dksample/src/native.rs) has a 64-byte
header, BE PCM and 16-byte trailer (header byte 1 stereo, +4 PCM bytes, +8 rate).
The runtime [mono decoder](../../modules/core/runtime/sdk/src/sample.rs) and
[job](../../modules/core/runtime/sdk/src/sample_job.rs) 8,192-frame bound are proof
restrictions, not product caps. The allocation-free
[stream parser](../../modules/core/runtime/sdk/src/sample_stream.rs) already checks
mono/stereo extents, `pcm + 80` and a caller maximum, but is not wired to source/transport.
[Source access](../../modules/core/runtime/src/rs/src/sample_source.rs) uses whole-file
DT2 FS reads without a stable lease/same-length replacement check; DN2 store uses
snapshot generation plus SHA. Stock DT2 pool planes differ from disk interleaving.
The converter's 64 MiB input/128 MiB PCM ceilings are software constraints, not RAM
ownership. Checked stereo extents, source consistency and an owned pool remain
open end to end; no new ABI, product limit or production backing is selected.

- [x] Route the nested standalone dkrt cleanup tests into the existing Rust job;
  default/whole tests include it, with the direct `test:runtime:unit` owner task.
  Runtime sources and local dependencies select it without losing existing readers.

### Route standalone runtime host tests

- [x] Add dkrt and its real local dependency edges to existing Rust affected-package
  selection, preserving workspace readers and one sequential gate job.
- [x] Split workspace/standalone execution; dkrt-only needs no generated SHARC core
  and uses a pinned, worktree-aware host target. Add the direct shared-lock owner.
- [x] Check routing/execution behavior, run the ten-test owner once and document
  the now-routed coverage without changing runtime sources or artifacts.

Routing validation passes 252 tests across the named selection/verify files and
the ten-test direct runtime owner. `mise run test:changed` passes fast checks;
`verify --only rust-tests --explain` retains one existing Rust job. No full gate,
runtime source/object rebuild or device acceptance is repeated for this routing delta.

### DT2 source consistency audit

**[D/O]** The bounded original DT2 1.17 filesystem audit confirms per-read
metadata/extent lookup and a nonexclusive open bit, not an immutable lease.
The audited RPC output constructor can ignore failed deletion and then open the
existing ID for writing; outer request admission remains incomplete and no race
was observed. See [finding 81](../findings/81-stock-sampler-dependencies.md).
Next audit mutation-under-mutex seams that could invalidate a partial load on
source change, independently of SHARC pool references. No hook, ABI, product cap
or production memory policy is selected; DN2 generation/SHA remains a separate path.

- [x] Audit reserve and alternate reclaim under the filesystem mutex: reserve
  bypasses write, and recursive/rollback reclaim bypasses delete or affects more
  IDs than its argument. File ID is available before first mutations, but not at
  lower bitmap helpers. This is CF source consistency, not DSP pool ownership.
- [x] Audit create/ID reuse: selected ID precedes bitmap/node mutation, all four
  recorded allocator calls hold the FS mutex, and failed linking can free the ID
  directly. Incarnation-like node metadata has no audited fail-closed wrap check.
- [x] Audit bounded format/mount/init paths: open-bit gating differs, mutex-reset
  init is unlocked, shared init is too late for sole global invalidation, and
  mounted 0→1 is not a generation.
- [x] Audit successful filesystem block I/O: three blocking waits precede successful
  completion, with no deferred flush in the checked write/read-cache paths.
- [x] Complete bounded error ISR/drain auditing: error events and channel interrupt
  clear do not locally abort/drain DMA; early errors skip waits and FS wrappers
  ignore observable block errors. No physical/controller-wide quiescence claim.
- [x] Trace full-block filesystem read pointers: DMA uses stock cache/bounce
  storage, then memcpy reaches the caller; no direct-to-caller branch or observed
  after-free write. This FS path alone does not justify retaining runtime input.
- [ ] Define lifecycle/content-change evidence and an independent low-level fault
  latch, with complete hook coverage and conservative uncertain-completion
  recovery. FS counts/header validation alone are insufficient; unknown completion
  is not assumed retry-safe, and no implementation/public ABI is selected.
- [ ] Complete init/indirect triggers, directory links, balanced open ownership
  and remaining writers before any source generation scheme; require fail-closed
  wrap/every affected ID. No hooks selected. See
  [finding 81](../findings/81-stock-sampler-dependencies.md).

### Read-only reachable SHARC store inventory

- [x] Extend the existing firmware query command with deterministic JSON rooted in
  explicit PCs, preserving actual edge targets versus containing functions.
- [x] Validate database schema/image provenance without rebuilding; account for
  every recorded store operand, pointer ambiguity and unresolved/out-of-scope edge.
- [x] Check synthetic read-only fixtures and one DN2 1.12 AudioTask report. This
  inventories static evidence/gaps, not runtime bounds, backing ownership or reserves.

The first DN2 1.12 report verifies explicit section-7 bytes against SHA-256
`336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`.
Requested `0x1c9fe7` lies in database function `0x1c9e76`; CALL-only whole-function
reach yields 93 functions and 2,278 recorded store operands/sites, including 1,002
stack-register candidates. Of 917 recorded edge rows, 171 CALL rows are followed;
23 unresolved CALL and 16 indirect rows remain explicit. No PM/dual operand occurs
in this bounded set; synthetic tests exercise both, without an absence claim.
These policy-labelled counts do not use earlier 3,229/729/1,136 totals as goldens:
roots/normalization, edge coverage, operand accounting and pointer ambiguity must
be aligned before comparing them. No width-to-byte or interval ownership inference
is made. See [command usage](../INSTRUMENTATION.md) and ignored
`out/runs/dn2-store-inventory/report.json`; coordinator review selects the next gaps.

The final named firmware-query file passes 25 tests, including read-only synthetic
cycles/interior targets, dual operands, pointer ambiguity, provenance/schema
failures and entry identity independent of edge normalization. Ruff, formatting
and mypy pass. Source/report identity and commands are retained in ignored
`out/runs/dn2-store-inventory/commands.json`.

- [x] Audit the requested root against original task creation/initial context:
  actual AudioTask entry is `0x1c9fe7`; database owner `0x1c9e76` merges adjacent
  dispatch code. All 23 missing-target-function CALL rows have concrete PCs
  (22 unique containing ranges, one unowned, zero ambiguous). Final tool review
  closed the entry-identity P2; prior source tests are not repeated for these docs.
- [x] Audit the bounded `0x1c125e` helper body: callable despite missing DB
  ownership, no stores/further calls in the followed ranges; adjacent entry and
  two other callers remain outside that body audit.
- [x] Establish incoming R4 provenance for caller `0xb80e54`'s `DM(I5+2)` store:
  valid slots 0..15 target voice-state word 2 at `0x2535e8 + 0x48*v`, not the
  external ring pointer/backing. The stride is `0x48` bytes (72 decimal).
- [x] Audit auxiliary DMA helper `0x1ca59f` direction/count setup: SPI1 DMA24 TX
  and DMA25 RX use fixed circular roots, XCNT=N>>1, XMOD=2 and two-byte widths.
  Valid nonzero count bounds a configured span, not actual size/coexistence;
  XCNT=0 is a configuration error.
- [x] Complete the bounded static lifecycle slice: callback `0x1ca6a7` reads the
  published auxiliary handle and reaches shutdown/control. No creator reference
  or linked descriptor start/count replacement was found; negatives are not dead
  code, and main SPI2's R4=`0xabc` is not auxiliary N.
- [ ] Resolve external/computed creator `0x1ca63e` entry/N, or capability-supported
  exact-entry R4 observation. Valid handle publication, external trigger, success
  and complete error cleanup remain open; no every-boot/perpetual activity or
  allocator/free-DDR inference is established.
- [ ] Keep external ring ownership and explicit entry/dispatch precision separate
  from this closed destination family; corrupted state and complete writer reach
  remain open. No traversal-policy or production reservation is approved. See
  [finding 106](../findings/106-dn2-ddr-configuration.md).

**[O] Parser wiring boundary:** the existing source contract supplies LE float32,
whereas `NativePcm16Stream` emits packed PCM16. Its existing canonical parser is
not a direct replacement for that job/consumer path. Production wiring awaits an
explicit representation and destination byte-extent contract plus source
consistency; no implementation, public ABI or product policy is selected.

The proposed storage-fault record must survive new loads. Its installation must
cover relevant cache I/O history or establish fresh/quiescent cache initialization;
a clear token after an unobserved failure is not health proof. Fault-capture and
recovery mechanisms remain unimplemented; the first-release policy is accepted below.

### Accepted decision: first-release sample-load recovery scope

The user accepted the following policy; the bounded audits above retain their
recorded limits:

- Missing, invalid or changed samples show an error, discard the unpublished load
  and leave the selected track silent. Another selection is allowed.
- An uncertain low-level storage fault with unproved DMA completion rejects the
  unpublished load and blocks new sample-load requests until reboot or
  independently proven recovery. A new load does not clear the fault record.
- The loader does not modify other published bindings; this does not guarantee
  underlying-device health. Automatic retry/recovery is not required in the first
  release.

- [x] Accept first-release recovery scope, distinguishing ordinary sample errors
  from uncertain low-level storage faults.
- [ ] Establish fault capture/history before relevant cache use and a lifecycle/
  content-change witness, then implement and validate the accepted behavior with
  complete hook coverage. No implementation completeness is claimed here.

Any future automatic retry is a separate larger scope requiring proven controller
abort/drain, cache invalidation, FS revalidation and bounded error injection.
The accepted policy itself proves neither DMA stopping nor production pool/hook
readiness and approves no reservation or public ABI change.

The prior documentation checkpoint passed auditor prose cross-checks, pathcheck,
`git diff --check` and `mise run test:changed`. This policy-only update runs docs
pathcheck and diff checks; no code change, emulator run, hardware experiment or
full merge gate is claimed.

### Next proof: private early DT2 storage-fault observer

This is shared runtime storage infrastructure, with the sampler as its first
consumer and reuse intended for the file manager/importer. The private DT2 proof
is not a production gate or DN2 implementation proof.

- [x] Bound the static early MAIN path: expansion-loader BSS zeroing precedes traced
  storage setup/ISR enable/first command, FS initialization and runtime task entry.
- [x] **[D]** Build the optional private patched-in DT2 observer: 39 assembled
  instructions and three passing compiled-source model tests. Static verification
  of the actual image segment, symbol/BSS extent and loader placement passed;
  runtime-loaded DKM BSS remains too late for the traced early history. Independent
  hook/model review is complete, and five host watch-validator tests pass.
- [ ] Complete the bounded native proof of initialization, sticky retention and
  controlled fault capture, including error plus completion in one ISR. Run
  `out/runs/storage-fault-proof/native/40cd90b5-0945-43af-9223-6626e5945f16`
  failed on the second boot's `main_changed: false`: the same MAIN image was already
  loaded. The assertion now expects `boot === 1`; it has not been rerun. Cold/cache,
  hash and observer assertions remain unchanged.
- **[D]** Both native traces place loader zeroing before FS initialization. The
  first readiness latch was zero, and the host seed `0x00310000` remained after
  one million ColdFire steps. There were zero observer writes, so these observations
  do not prove IRQ fault capture or complete early/reset history.
- **[D]** The native runtime constructs the board with `CompletionPolicy::Oracle`
  (`packages/emulator/boot/src/runtime.rs:483`). `complete()` directly posts
  DMA/data/command semaphores and clears status
  (`packages/emulator/machine/src/board.rs:1075`); `drain_board()` counts the events
  (`packages/emulator/boot/src/runtime.rs:2249`). Default native storage completion
  therefore does not establish stock ISR execution.
- [x] **[D]** Execute the actual stock DT2 ISR, real no-waiter signal helpers and
  RTE with assembled hooks in the opt-in
  [component fixture](../../packages/patchkit/tests/storage_fault_isr.rs).
  All 36 sequence/SR combinations pass: eleven single-interrupt cases and one
  five-interrupt sequence, each at three SR values (48 interrupt invocations per
  image). Synthetic vector/level injection and a controlled W1C IRQSTAT double
  compare stock/patched context, exact MMIO and nonobserver write traces,
  status/signal globals and sticky error union, including error plus completion.
  The [adapter README](../../modules/core/adapters/storage-fault-proof/README.md)
  gives the opt-in `DIGI_STORAGE_FAULT_ISR_SYX` command and strict input hash.
- [x] **[D]** Deliver through real eSDHC/INTC registers and the actual ISR/waiter
  on DT2 1.17 and DN2 1.12 from explicitly initialized state. The controlled B
  component below proves ready-queue insertion and a scheduling request, with
  stock/patched status/context equivalence; it does not execute the resumed task.
- [x] **[D]** Complete the controlled eSDHC/DMA59 pair checkpoint below: genuine
  guest DMA copy, both actual ISR orders and three waiter insertions, with
  stock/patched selected-observable equality on both target images.
- [ ] Complete the full native fault-history acceptance, which remains
  **failed/open** because zero observer writes were observed. Controlled component
  success does not establish native-boot IRQ execution, hardware delivery, physical
  DMA quiescence, resumed task/full-scheduler behavior or complete initialization/reset history.
  Do not weaken observer assertions, change emulator semantics or switch wholesale
  to Device mode to obtain a pass.
- [ ] Establish complete relevant fault history and source lifecycle/content-change
  witnesses before any production sample-load gate. No pool/reservation/public ABI
  decision follows from this private proof.

The earlier all-changed `mise run test:changed` wrapper failed on the pnpm
dependency layout. The later explicitly scoped changed-file check for the new ISR
fixture and these four documentation files passed pathcheck/layers; the full merge
gate was deferred and has not run. The code/model and bounded observations above
remain **[D]**, not **[V]**.

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


**[D/O] Bounded scheduler prerequisite complete (2026-10-31):**

- [x] Manual and runtime-policy/snapshot audits completed for the bounded decision.
- [x] Explicit `step_storage_with_forced` combines real storage lines with all
  software force sources, selecting higher level, lower bank, then higher source.
  IMR/nonzero ICR/live IPL apply; unsupported controller level masking and level 7
  fail before mutation. Pair-only/Oracle/default routes remain unchanged.
- [x] 28 component tests pass (13 INTC, 15 storage machine); direct higher-level
  bank2-over-bank0 assertion passes in its subsequent named-test rerun.
- [x] Actual controlled scheduler continuation passes 8 runs: both exact images,
  stock/patched, CC/CC+CTOE. Real vector 223 requests bit 13; vector 205 executes
  the actual scheduler `0x40000410` and RTE `0x40000458`, restores one initialized
  waiter's exact context and executes its guest marker instruction. Old-task
  register save, selected SP/SR/PC, singleton queue, semaphore state, force clear,
  W1C and stock/patched equivalence are asserted. All four named firmware tests
  pass with both opt-ins; independent static review found no issues.
- [ ] Snapshot policy binding remains open: Board/Time retain register backing,
  without CompletionPolicy/TimerPolicy/construction binding; native boot hardcodes
  Oracle and checks firmware identity. Runtime execution-policy compatibility is
  unaudited; no exploited mismatch is claimed. No format migration is approved.
- [ ] Full reset, physical-source/timer/full-scheduler integration and native-boot
  fault-history acceptance remain open. Stage C daemon integration is unstarted.

Exact invocation, fixture/input hashes and log are under ignored
`out/runs/storage-fault-proof/scheduler-continuation-invocation.json` and
`scheduler-continuation-tests.log`; component logs/provenance use `storage-forced-*`
in the same directory. These are controlled initialized-state proofs, not a
native boot or complete relevant fault history. The coordinator's scoped
`mise run test:changed` for the five arbiter Rust files, firmware fixture and plan
passed pathcheck/layers before final prose; final docs pathcheck is coordinator-owned.
The full merge gate remains deferred. See [finding 107](../findings/107-storage-interrupt-emulation-scope.md)
for boundaries and evidence.

Remaining: resolve Device runtime construction and snapshot design, execution-
policy/topology compatibility and reset before exposing Stage C. Existing Oracle
snapshot usability must be preserved.


**[D] Legacy runtime snapshot guard complete (2026-10-31):** native runtime
`save_state`/`load_state` now require Oracle completion and attached Oracle Time
before any CPU flag resolution, time-base change or deserialization. Device
completion, Device Time and missing Time fail explicitly. Both named tests pass
on exact DT2 1.17 and DN2 1.12: Oracle `DT2SNP01` magic and byte-identical roundtrip,
and unsupported save/restore rejection preserving CPU, RAM, registers, deadlines
and counters. Independent static review found no issues. Log and exact command,
environment/input hashes: `out/runs/storage-fault-proof/legacy-snapshot-policy-tests.log`
and `legacy-snapshot-policy-invocation.json` in the same directory.

This preserves the legacy format and Oracle defaults; it does not complete policy
binding. Reference/Softfloat interchange and topology compatibility remain unknown.
Device runtime snapshots remain unsupported; no migration, cache-key change or
Device runtime option is added. Future Device construction must start cold until
its snapshot contract is separately validated. Full-reset implementation and native integration remain
unstarted; the full merge gate remains deferred. The coordinator's scoped
`test:changed`, final documentation pathcheck and whitespace checks passed.

### Native integration scope decision — 2026-10-31

The read-only runtime audit found separate ordered delivery paths for timers,
software-force requests, SSI and other services. Adding storage to one path
would not establish correct simultaneous-source priority. Fast execution windows
also need to end when storage request state changes. The reusable INTC candidate
selector is ready, but composing physical source eligibility and acknowledgement
is still open; Device timer service currently rejects delivery.

- [x] Audit the native boundary and identify why a storage-only mode switch is insufficient.
- [x] User approved correctness-first shared interrupt arbitration before further
  sampler integration (2026-10-31). The scope question is resolved.

This scope choice does not authorize default-policy changes, hardware experiments,
snapshot format migration or weakening the native fault-history assertions.

Implementation sequence: separate existing timer advancement/latching from
delivery first, preserving Oracle behavior; then compose physical requests with
storage and software force before a single selection/entry boundary. Timer
status/acknowledgement semantics require manual-backed Device validation before
claiming native integration. SSI, host input and fast/batched execution boundaries
must participate too. Native TX35 uses vector 155 (INTC1 source 27), not DTIM3's
vector 99. Panel RX is 154 and MIDI RX is 156.

- [x] Expose non-delivering timer pending-request collection with Oracle regression checks.
  PIT/DTIM advancement is separated from delivery; 30 named integration/manual
  timer tests and three existing Time cache tests pass. Independent review found
  no issues; coordinator path/layer and whitespace checks passed.
- [ ] Establish Device timer status and guest acknowledgement behavior from the manual.
- [ ] Compose physical sources and one shared delivery decision, including batching boundaries.
- [ ] Validate explicit native storage execution and fault history on both firmware images.

Payoff: trustworthy storage-failure tests while the normal runtime remains stable.
Confidence is high for the timer separation, moderate for the wider integration;
full reset behavior and physical-source interactions remain unresolved.

### Device PIT explicit event component — 2026-10-31

- [x] Implement supplied timeout events, register-backed PIF/PIE requests and
  aligned 16-bit PCSR/PMR acknowledgement, from MCF5441XRM p1156/p1157/p1159.
- [x] Pass 26 named component/regression tests and all four actual-firmware
  storage tests with both exact images. Seeded PIF survives storage ISR/RTE;
  the actual scheduler's `$053f` word write clears it on both devices.
- [x] Independent review found no issues; scoped coordinator path/layer and whitespace checks passed.

This does not enable automatic timing, counter/phase/reset modeling, DTIM or
shared/native delivery. Oracle behavior remains unchanged. Unsupported Device
PIT access widths and registers reject before register mutation. The existing
storage/force route excludes physical PIT requests. Logs and exact invocations:
`out/runs/storage-fault-proof/pit-device-events*`. The full merge gate remains
deferred; this is supplied status/acknowledgement evidence, not timer IRQ timing.

**[D] DTIM3 discovery:** MMIO database references located setup/handler at
DT2 `0x400c0b6e`/`0x400c0b40` and DN2 `0x400bb19e`/`0x400bb170`.
Both setup bodies install vector 99, select level 2, clear DTCN, set DTRR to
`0x43238` and DTMR to `0x001d`. Each handler writes byte 2 to DTER and calls
`0x40001896`. No DTXMR write appears in those setup bodies; the complete mode
history and repeated-reference rearming remain open. The earlier bounded
first-64-KiB vector-slot scan missed these later functions and is not absence evidence.

### Device DTIM reference-event component — 2026-10-31

- [x] Implement supplied REF events, reference-only CPU requests, bounded register
  configuration and DTER W1C from MCF5441XRM p1164–p1168.
- [x] Pass 28 named component/Oracle regression tests and all five actual-firmware
  storage tests with both exact images. The new vector-99 prefix case has four
  runs: DT2/DN2 and REF/REF|CAP, each bounded to 64 instructions, stopping before
  the actual `0x40001896` queue-helper call. REF clears and CAP is preserved.
- [x] Independent review found no issues; coordinator scoped path/layer and whitespace checks passed.

Capture delivery, automatic timing/rearming, reset, DMA execution and shared/native
arbitration remain excluded. Device construction initializes DTRR to `0xffffffff`
only; Oracle defaults stay zero, and no full reset fidelity is claimed. Unsupported
capture/advanced modes, reset transitions and live nonzero CLK changes reject.
Exact commands/hashes/logs: `out/runs/storage-fault-proof/dtim-device-events*`.
The handler-prefix proof establishes no complete ISR/RTE or task behavior; the full
merge gate remains deferred.

### Shared supplied-event selection — 2026-10-31

- [x] Add explicit `Machine::step_device_events`, combining supplied PIT and DTIM
  reference requests with storage lines and all software-force sources before one
  shared INTC selection. Reuse storage preflight/entry/cache handling.
- [x] Pass 66 named tests (44 machine, 13 INTC and nine peripheral event cases
  selected by the shared Cargo target names), plus all five actual-firmware
  storage tests with both exact inputs and unchanged fixtures.
- [x] Independent review found no issues; coordinator scoped path/layer and whitespace checks passed.

Existing stepping routes retain their contracts; only the explicit new method
includes supplied timer status. Sources persist until guest acknowledgement;
PIT/force aliases share one source and either assertion can keep it pending.
Invalid DTIM modes reject before CPU/queued-event/cache-invalidation mutation.
No automatic timing, physical SSI/DSPI, host input, batching or native integration
is added. The broader composition milestone remains open. Exact commands/hashes/
logs: `out/runs/storage-fault-proof/device-event-arbitration*`; full gate deferred.

### Device SSI50 latch and shared selection — 2026-10-31

- [x] Latch qualifying supplied SSI TX50 major completions in sticky/coalescing
  INTH bit 18 after successful service; include vector 170 only in explicit
  shared Device-event selection. Queue Device RX invalidation ranges.
- [x] Synchronize CINT/CAIR/INTH W1C acknowledgement with SSI diagnostics;
  Device CINT NOP returns before legacy callbacks, preserving pending DSPI too.
  Machine consumes paired invalidation tags only when draining their ranges.
- [x] Pass 61 named machine tests and all five unchanged actual-firmware storage
  tests with both exact inputs; independent review found no issues.
- [x] Coordinator scoped path/layer and whitespace checks passed.

Oracle scheduling/delivery and existing storage routes remain unchanged. The
caller explicitly supplies due SSI service; no automatic SSI clock, native
service/batching policy, DSPI or host-input delivery is enabled. Correct-target
final commands/hashes/logs: `out/runs/storage-fault-proof/device-ssi-events*`.
A mistargeted initial build is separately archived and not final validation;
metadata records its actual target and archive. The full gate remains deferred.

### Device SIMR/CIMR byte contract — 2026-10-31

- [x] Complete all three banks' source 0–63 and SALL/CALL mask commands, zero
  byte readback and refusal of reserved-bit/wrong-width/overlapping accesses
  before register/cache mutation. Preserve Oracle passthrough, IPR composition
  and controller level-mask behavior.
- [x] Pass 15 INTC manual, four new mask-command, eleven Time, 23 storage-pair,
  nine eSDHC and ten SSI tests (72 total), plus four Time cache unit tests.
- [x] Independent review found no issues; coordinator scoped path/layer and whitespace checks passed.

Exact commands/environment/source hashes/logs:
`out/runs/storage-fault-proof/intc-mask-commands*`. No firmware rerun was required
for this mask-only patch by coordinator decision; previous SSI firmware results
remain previous-checkpoint evidence. No native policy or snapshot format change;
the full merge gate remains deferred.

Diagnostic mixed-policy native integration may retain explicit host peer
producers, but cannot replace or weaken existing full fault-history acceptance.
Synthetic vector-86 acceptance causing vector-65 creation is a modeling assumption
requiring lifecycle/batching tests. Full EPORT is not a prerequisite; RSTA remains
open. This component does not enable native integration or change those gates.

Private mixed-policy diagnostics must reject USB MIDI callback injection,
including prequeued or active callback state, until a distinct admission contract
exists. USB and DIN share vector 156, but the current USB callback uses fixed
level 5 outside ICR/IMR; USB must not be silently represented as DIN source 28.
Oracle USB behavior is unchanged; this is a future diagnostic admission decision,
not native implementation in this mask component.

**[D/O] Ordinary DTIM timing direction:** manual p1169 describes FRR restart,
and p1171–p1172 §39.4.1 polls five successive REF events after enabling once,
clearing DTER byte 2 without DTRR updates or reset. This strongly supports intended
ordinary repeated-reference operation. Interpreting Table 39-4's generic rearm
restriction as EN1588-specific matches qualified §39.2.4, but remains an
interpretation of conflicting prose, not hardware readback. Eq. 39-1 (p1172) gives
clock period × divisor × (PS+1) × (DTRR+1). This supports a future explicit-clock
ordinary-mode component; current bus frequency/phase and full firmware DTXMR
history remain unproved. No timing implementation or [V] claim is added here.

### Device DTIM supplied post-prescaler ticks — 2026-10-31

- [x] Implement ordinary restart-mode DTCN/REF advancement from explicitly supplied
  completed post-prescaler ticks, using existing register state only.
- [x] Pass six new peripheral tick, five peripheral event, ten DTIM manual, five
  machine DTIM, eleven Time and 23 storage-pair tests (60 total).
- [x] Independent review found no issues; coordinator scoped path/layer and whitespace checks passed.

**[D] Completed-tick phase:** p1166 §39.2.4 explicitly states DTRR+1 intervals,
and p1163 FRR resets DTCN immediately after reference. Thus DTRR=2 advances
0→1→2→0+REF at this completed-tick abstraction. The earlier five-poll example
supports repeated ordinary timeouts separately. No subcycle, simultaneous-MMIO
or real hardware cycle accuracy is claimed.

Only active DTXMR=0/CE=0/FRR=1/RST=1/internal-CLK states advance; ORRI gates the
request, not counting. Inactive RST/CLK preserve state, unsupported advanced modes
reject first, and active DTCN>DTRR rejects. Wide arithmetic handles u64 ticks and
u32 reference maxima. CAP/other event bits survive; multiple periods coalesce.
No implicit write advancement, bus/instruction-clock conversion, new snapshot
fields or native consumer is added. Commands/source hashes/logs:
`out/runs/storage-fault-proof/dtim-device-ticks*`. No firmware rerun is required
absent a new consumer or uncertainty; prior results remain prior evidence.
The full merge gate remains deferred.
