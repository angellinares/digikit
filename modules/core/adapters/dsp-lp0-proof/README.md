# Private DN2 LP0 receive and reply-extension diagnostic

Version 0.5.0, explicit opt-in for the emulator, DN2 OS 1.12 only. Requires
`core/expansion`, `core/machines`, `core/params` and `core/dsp-voice`; conflicts with `core/dsp-link`, `core/runtime` and
`core/dsp-ddr-proof`. Absent from device module sets. Never use on hardware.
The earlier 0.1 memory-observed proof is retained in its committed checkpoints.

Both wire counts change **at boot only**, `0xabc` to `0xacc` (16 extra bytes).
The CF boot getter call is retargeted; the original getter is unchanged. DSP
initialization receives the new length before constructing all four SPI2
ping-pong descriptors (`XCNT=0x566`, stride two). No armed descriptor is resized.
Numerical buffer capacities admit this experiment; exclusive gap ownership and
hardware safety remain unproved. See [reply ownership](../../../../docs/findings/88-dn2-reply-ownership.md).

The stock CF memcpy still copies exactly `0xabc` bytes. Its destination ends
exactly where stock TX starts, so extending that copy would corrupt stock data.
A wrapper at that memcpy call preserves its arguments and return value, then
reads the separate extension from source `+0xabc` into module-owned storage
before the frame driver rearms DMA. No header, row or trailer bytes are reused.

The extension remains eight numeric 16-bit halves. For LP progress, in order:

| Half | Meaning |
| --- | --- |
| 0, 1 | Magic `0x4c50`, version 1 |
| 2, 3 | Status, error |
| 4, 5 | Pages, low half then high half |
| 6, 7 | Bytes, low half then high half |

Version 0.3 multiplexes the last reply kind into those same 16 bytes. CLOSE
replies are four numeric words `{0x00014241 | status<<24, id, track, epoch}`,
with status PENDING=1, DRAINED=2, REJECTED=3. Each word travels low numeric half
first, so header halves are `0x4241`, `0x0100*status+1`. Global driver ERROR
always overrides CLOSE and publishes the LP progress error kind. CF decodes
progress and CLOSE into **separate** sequence-protected snapshots; observing one
kind retains the other. Unknown/malformed matching CLOSE replies fail closed.

The DSP stores four words using the existing low-half-first wire convention.
It captures status/error/pages/bytes under the SDK local interrupt mask, clears
SIMD for scalar stores, and publishes at the common tail after setup/rearm work
has returned. Exact MODE1 and saved registers/DAG state are restored, bases
before indices. This CPU critical section does not establish general DMA ownership.

A CF ISR seqlock protects the local decoded snapshot `lp_ack`: sequence, valid,
status, error, pages, bytes (six u32 words). Identical observations retain the
sequence; it is a private local coherence counter, not a wire generation API.
Wrong/absent magic remains pending. Valid magic with unsupported version,
inconsistent progress or DSP error fails closed. Malformed/error observations
remain sticky even if a later reply looks valid. Old valid progress stays pending.

`lp_mail` remains four u32 words: state, error, tag, sent count. The host writes
all 1,024 numeric BE staging words and the tag, then publishes START=1 last.
Before sending, the UI requires a coherent CF ACK with status 2, error 0 and
pages/bytes exactly matching the current cursor. A send-once flag is set before
the stock sender. BUSY=2 retains staging through sender return and subsequent
ACK polling: DONE=3 now means **DSP acknowledged consumption**, not merely sender
return. Post-ACK must match the next page and byte count, status 2 for intermediate
pages or status 3 for terminal page 235 / 960,000 bytes. Version 0.3 rearms after
that terminal payload too, retaining status 3 on success, to receive CLOSE. This
intentionally supersedes 0.2 terminal-inactive behavior; old runs retain their
historical meaning. Invalid requests/errors
produce sticky ERROR=4. There is no teardown, retry, reopen or retirement on timeout;
errors require a fresh boot. The stock sender itself still has an unbounded
readiness wait, so the external runner must bound its owned emulator.

LP0 setup still requires live IMASKP, owner, LP0_CTL and DMA30_CFG zero before
mutation. Private aligned L2 state/descriptor/RX storage, sequential tags 0..234,
the fixed unpublished bank `0x80800000..0x808ea600`, final 1,536-byte clamp, and
both full 4 KiB canaries remain. `_lp_status/error/pages/bytes` are independent
emulator oracles; native test progress comes from the CF wire ACK.

Version 0.2.1 introduced independent RX alignment to 64 bytes within a
4,224-byte reservation, retained by 0.3.
The 4,100 DMA bytes (tag plus 4,096-byte payload) occupy a rounded 4,160-byte
footprint wholly owned by RX, disjoint from the descriptor and saved context. Driver state remains
128 bytes with 16-byte alignment; descriptor logic is unchanged. This layout is
necessary for whole-line ownership but supplies no cache coherence or maintenance.
The 0.2 native runs retain their historical addresses and evidence only. Version
0.3 requires a native boot fixture bound to the exact new patched-image hash;
the passing 0.3 run below uses that fixture.

## Private CLOSE command and diagnostic drain controls

LP0 tag `0x01014243` carries a fixed 1,024-word payload beginning
`{request_id, track, expected_epoch}`. CF uses a separate zero-initialized
staging buffer; unused padding has no DSP semantics. CLOSE never copies sample
payload or advances pages/bytes. IDs/epochs are positive signed 31-bit values;
tracks are 0/1. Lower IDs, conflicting identical IDs and higher IDs while pending
reject without replacing accepted history or changing controls. An exact retry
rechecks the live epoch and drain; a new accepted ID clears the prior drained
grant. No wrap is permitted.

Two private six-word records expose open, ready, closed, count, fault
and epoch (`_lp_close_{a,b}_FIELD`). In 0.4 actual readers share these records.
Both epochs initialize to 1; A initializes unready/open 0, closed/count/fault 0.
B publishes only after its owned buffer initialization. SDK close/drain
macros close admission and check exact count/fault. The callback rearms before
its result becomes eligible for tail publication; rearm failure is global ERROR.
The AudioTask tail checks a pending accepted request once per frame under its
local mask, after scalar normalization and with full hook state restored on
exit. It never spins, calls a driver or resends. Full incoming MODE1/bank selectors
are retained; scalar status tests occur after disabling SIMD. Callback entry
uses the existing scalar stock calling convention and preserves its MODE1,
callee registers and stack while masking the SDK control operations.

`lp_close_mail` has six u32 words `{state,error,id,track,epoch,sent}`. Publish
START last and retain all request fields until DONE/ERROR. CF captures an
immutable tuple; coherent LP readiness status 2 or 3 permits one send. Sender
return and LP progress cannot complete CLOSE. Exact PENDING stays BUSY; only
exact DRAINED completes. Wrong ID/track/epoch is ignored. Matching malformed,
REJECTED/unknown status and a budget of 65,536 UI polls cause sticky quarantine;
late ACKs cannot recover it or authorize buffer reuse. ISR-visible active identity
is volatile and published last. A matching bad observation is latched in the ISR,
so a later valid frame cannot erase it before UI polling. On exact DRAINED, CF
first retires the active tuple (terminal claim), then checks sticky evidence before
publishing DONE. Subsequent tuple replies cannot change that terminal outcome.
This poll budget is not
a wall-clock bound; the stock sender still requires the external deadline.
CLOSE quarantine/global errors fail both mailboxes and retain buffers until boot.

One global sender owner serializes both paths. In-flight data finishes first;
otherwise queued CLOSE has priority over queued data. No data begins while CLOSE
is active. Separate bounded one-attempt CF seqlock reads and the IRQ-masked DSP
snapshot cover this diagnostic's publication; READY-last alone is not a coherent
multiword message protocol. No public ABI or physical cache safety follows from these controls. The separate
22-case logical fixture remains independent of this transport and the real readers.

## Required private boot fixture

The stock `cold-idle.dsp` already contains four armed/queued `0x55e` descriptors.
Overlaying boot instructions onto it cannot rerun their initialization. A private
pre-init-derived cold-idle is mandatory; live resizing is forbidden.

After building/applying the exact current diagnostic to an ignored `reply.syx`,
prepare a private DSP patch with the existing tool:

```sh
uv run python -m tools.patchkit.pk_dsp_diff firmware/Digitone_II_OS1.12.syx PRIVATE/reply.syx PRIVATE/reply-dsp.patch
```

Run the existing `dsp_cold` executable under the shared build/measurement lock
and an external **120-second wall timeout**, with:

```text
DSP_DEVICE=dn2 DSP_PATCH=ABS/PRIVATE/reply-dsp.patch SHARC_FAST_REGIONS=""
dsp_cold ABS/STOCK/digi-audio-dn2-image.bin ABS/STOCK/cold.state ABS/PRIVATE/cold-idle.dsp 800000000
```

This uses the original pre-init `cold.state` (PC `0x1c12e2`, empty overlay), so the
patch is applied before execution. Reuse the verified existing generated core;
stop if a tool requests core generation. Exit code zero is insufficient: the tool
also writes a state after halt or limit. Require its actual `idle:` log marker,
matching exported PC in `[0xb88a49,0xb88abb]`, and inspect the new state **before
any CF exchange** for all four fully known XCNT values `0x566`:

| Direction | Descriptor root, host byte address | XCNT field, host byte address |
| --- | --- | --- |
| RX0 | `0x282c2960` | `0x282c296c` |
| RX1 | `0x282c297c` | `0x282c2988` |
| TX0 | `0x282c2998` | `0x282c29a4` |
| TX1 | `0x282c29b4` | `0x282c29c0` |

Clone the stock ready inputs into a private directory. Retain the original
CF-ready snapshot, ready DSP snapshot, packed DSP image and stock `SOURCE.json`.
Replace **only** the clone's `cold-idle.dsp` with the pre-init-derived result.
The packed DSP image must be a physical byte-identical copy inside the private
directory, not a symlink back to stock inputs: the daemon canonicalizes that image
path and finds `cold-idle.dsp` beside it. Other unchanged inputs may remain links.
Initial Session stock restore still uses the original ready pair; patched cold
startup uses the private cold-idle and matched patched CF boot length.

Set both `DIGI_PK_READY_DN2_112=ABS/PRIVATE` and
`DIGI_LP0_BOOT_FIXTURE=ABS/provenance.json`. The read-only schema is defined by
`BootFixture` in the [native test](../../../../tests/regression/runtime/sample-lp0-transfer.test.ts):

- `schema: 1`, absolute `readyDir`, `baseSha256`, exact `patchedSyxSha256`.
- `{path, sha256}` for `dspPatch`, `packedDspImage`, `originalReadyDspSnapshot`,
  `dspColdBinary`; all paths absolute and files present.
- `coldState: {path, sha256, pc: 0x1c12e2, overlayBytes: 0}`.
- `coldIdle: {path, sha256, pc}` naming `readyDir/cold-idle.dsp`.
- `preparation: {command: string[], environment: {DSP_PATCH, DSP_DEVICE: "dn2"},
  maxInstructions, wallTimeoutSeconds, idleReached: true, log: {path, sha256}}`.
  Command records the executable, image, input/output states and explicit limit.
- `preExchange.descriptors`: four `{rootHostByteAddress, xcntHostByteAddress,
  xcnt: 0x566, knownMask: 0xffffffff}` observations at the table above.

JSON numbers use decimal notation. Record actual commands, hashes and observations;
do not populate metadata from expected constants. The test hashes every input,
checks idle evidence/count audit, preserves the original ready snapshot identity,
and binds the exact new patch output SHA. It never builds or prepares this fixture.
When native opt-ins are enabled without it, collection fails clearly.

## Validation limits

Focused assembled checks: `uv run python -m pytest tests/patchkit/test_dsp_lp0_proof.py -q`.
All 73 focused cases pass, including 16 four-byte data-placement phases checking
RX alignment, complete footprint ownership and padding preservation.
Stand-ins test our guards/call frames, halfword decoding, stock-copy boundary,
seqlock and send-once completion behavior. They do not execute the real driver/ISR
or prove cache behavior, gap ownership, DMA bank ownership or hardware safety.
The native first-page, full-bank and held-stock-audio cases retain independent
bank/hash/canary checks and now require armed terminal receive under 0.3.
The additional opt-in `DIGI_LP0_CLOSE=1` selects the test named
`closes diagnostic reader control over real LP0 and drains without resend with held stock audio`.
It uses the same exact-hash boot-fixture validation and existing Session harness,
transfers the full bank, injects diagnostic A count=1 before CLOSE, observes
BUSY/PENDING/A closed/B unchanged, then clears count and waits for exact DRAINED
without resend. Bank, canaries and finite/non-silent held stock audio are checked
afterward. This is explicit diagnostic fault injection, not a paused native voice.
Do not reuse a 0.2/0.2.1 boot fixture.

The explicit 0.3 CLOSE case passed on 2026-10-30: one pass, three skips,
60.350-second selected test, exit 0. Artifacts:
`out/runs/lp0-proof/27dbeea7-d1f2-415a-b98a-0a75c25fc3d1`; log:
`out/runs/lp0-close-fixture/native-close.log`. Exact patched-image SHA-256:
`6ff3b5ec9d9ed80f1108746922a213a75c9827a867fb257c1690c419fa56a2c8`.
All 235 pages / 960,000 bytes, exact diagnostic drain without resend, bank/canary
checks and finite/non-silent before/during/after held-stock-audio windows pass.
This is real LP0/stock reply transport with injected count 1→0, not an actual
paused native audio reader or other-track PCM proof. Only the final CLOSE ACK
snapshot is retained; intermediate assertion limits and the passing independent
native artifact audit are detailed in [finding 94](../../../../docs/findings/94-sample-binding-lifetime.md).
No production backing/cache, hardware, dropout-free audio or performance claim follows.
The historical 0.2 first-page and full-bank held-stock-audio
proofs pass with a verified private boot fixture; artifacts and remaining limits
are in finding 88. No public production completion contract follows from this proof.

Current 0.3 owner checks pass: focused Python tests and Ruff; scoped TypeScript
format/lint/types; generation, current-object and document path checks. Module-owned
CF/DSP objects re-decode successfully (450/727 instructions), built with the existing
patchkit under the shared build lock. No emulator/core build occurred for this
checkpoint; the native CLOSE run used the existing prebuilt daemon.
For a shared prebuilt CF test runner, explicitly set `DIGI_LP0_CF_CALL`; otherwise
the tests use `pk_cfcall.EXAMPLE` in the checkout and never implicitly build it.


## 0.4 actual two-track diagnostic readers

The explicitly selected DN2-only `lp_sampler` machine reserves private type 12.
Its Reverse knob selects forward/backward bounded traversal. `dk_voice_in.track`,
never the DN2 physical voice slot, selects A (track 0) or B (track 1); other tracks
return DONE and zeros. Nine state words hold the six SDK cursor words and cached
epoch/source/track. The real module callbacks use the existing SDK PCM16 kernel.
Note-on pins descriptor reads; render pins source access. A masked admission,
owner and epoch check precedes acquisition, and the exact MODE1 is restored before
accessing descriptors or PCM. Release follows the final access. There is no note
lifetime pin. Corrupt accounting poisons the count and closes admission.

A uses the retained fixed private 960,000-byte experimental bank at `0x80800000`:
480,000 frames at Q16 rate 65536. All 235 pages, full guards and successful
terminal rearm must precede immutable descriptor publication and READY/open.
CLOSE before or during loading sets sticky CLOSED; epoch 1 never reopens.
Rearm failure cannot publish. Buffers remain until boot; no replacement exists.
A's production ownership and hardware cache safety remain unproved.

B uses one 16,512-byte module-owned reservation: a 64-byte lower guard, 16 KiB
PCM16 payload (8,192 frames), then a 64-byte upper guard. Exported normal-word
`_lp_b_lower`, `_lp_reader_b_source`, `_lp_b_upper` pointer values establish the
layout independently of object section order. Its eight-frame triangle is
`[-8192,-4096,0,4096,8192,4096,0,-4096]`, repeated after initialization. Q16 rate
1024 gives 10.9227 seconds and 93.75 Hz. No additional DDR range is used.

Per-track `_lp_reader_{a,b}_{notes,blocks,reads,state,output}` observations support
the native diagnostic; `reads` counts admitted SDK kernel invocations, not memory
transactions. Descriptor budgets are conservative instruction ceilings: 2500 for
render (SDK ceiling 2662, measured 1854 without the loop option, plus bounded wrapper), 300 for note-on, depth 2 and
128 stack bytes. These historical `cycles` fields are not hardware timing claims.
New initialization/zero loops use software branches, with no new hardware loop.
The bridge retains machine JUMP I12/I13, the kernel stack slot and exact MODE1.

`DIGI_LP0_VOICES=1` selects the actual two-track case in the existing native test.
It transfers known 200 Hz A PCM, selects both machines using live menu positions,
and first requires independent source-track dispatch and concurrent counters.
It stops on an ambiguous panel/held-note mapping. It then sends actual CLOSE A
without reader-count injection, retains before/intermediate/final mail/ACK/control
snapshots, and requires A reads to stop while B continues with its expected direct
block and bounded 93.75 Hz WAV component. PENDING need not be observed between
blocks. Mixed stock FX tails are retained. This case needs a new exact-hash 0.4
boot fixture; **native execution remains pending parent preparation and review**.
The injected-count 0.3 case remains separate. Historical 0.3 hashes/results above
refer only to that checkpoint and must not be reused for 0.4.


### Raw callback observation correction

The first 0.4 native attempt established both source-track dispatches and actual
CLOSE/drain, then failed an assertion that A's retained stock output buffer stayed
zero. The DN2 gain/envelope/filter/overdrive chain subsequently modifies that buffer
in place. It is not an immutable raw callback result; the earlier failed run and
fixture remain historical evidence, not a passing 0.4 milestone.

Each track now owns a private 32-float callback snapshot and
`_lp_reader_{a,b}_snapshot_seq`. Sequence 0 means no block has been emitted; a
writer publishes odd before copying and the next even value afterward. A snapshot
is copied immediately after the SDK kernel or after this module's denied zero-fill,
before returning to stock processing. The native reader obtains the adjacent
sequence and 128 bytes in one synchronous DSP-worker peek, accepting only a
positive even sequence newer than its baseline. Counters are monotonic
within the bounded diagnostic run; wrap is not a supported observation interval.
This private observation adds no backing lifetime pin or public ABI.

The native test requires A's fresh zero snapshot to advance from its pre-CLOSE
sequence and B's coherent raw triangle snapshot to advance after drain. External
output pointers remain distinct observations, without immutable-content assertions.
The mixed WAV's post-drain component still starts at the separate capture-origin
marker after exact DRAINED/count zero. The 2,500-instruction render budget includes
the SDK ceiling 2,662 (measured 1,854 without the loop option) and the bounded wrapper/snapshot copy; note budget remains
300, depth 2 and stack 128 bytes. No hardware timing claim follows. A new exact-hash
boot fixture is required for these new DSP bytes. The corrected native run
`16929724-3d8c-49b4-bc79-b2161af79af9` passed with the exact v2 fixture;
see finding 94 for hashes, raw snapshots, post-drain audio and proof limits.


## 0.5 private startup DMA window

At the named boot cache-enable hook, the private adapter derives one aligned
4,352-byte window within a 4,416-byte module reservation. The mutable 28-byte
LP0 descriptor starts at offset 0, the 128-byte state at offset 64, and the
4,100-byte RX at offset 192, rounded to 4,160 bytes. Metadata and saved contexts
remain outside the entire window. `_lp_dma_base` and `_lp_dma_limit` export the
expected half-open interval; `_lp_descriptor`, `_lp_state` and `_lp_rx` are pointers.

The hook first tests and clears SIMD without a scratch register, then refuses
an incoming SIMD context before any saves or calls. It restores that MODE1 bit
and resumes stock boot with status 0 (unconfigured), denying LP0 admission.
For SISD entry it guards M5=0, M6=1, M7=-1, M14=1 and L4=L5=0, and calls the existing stock L1 range helper with selector 0x604, mode 1, base
and limit minus one. It retains the existing startup stack and circular DAG
configuration and restores live registers and exact MODE1 before replaying the
displaced CFG load. Boot status 1 means the helper returned zero; status 2 with
`_lp_boot_error` records either the helper error or context error 0x100. A missing
or failed boot setup prevents LP0 open, submit and both reader publications.

The L2 hook adds no cache-MMR reads. Native acceptance reads exact RR4 start/end
and CFG2 through `dsp.peek_word` and requires the expected interval and enabled
noncacheable pair 0x300. It needs a new exact-hash 0.5 boot fixture with corrected
range-register metadata. Owner stand-ins verify our call frame, preservation,
fail-closed behavior and layout; they do not execute the stock helper. The native
0.5 proof passes in run `b734a462-c9b0-4a67-a5a4-e0982c845d27`, including actual
two-track retirement and cached ready-pair restoration (finding 95). Production
RR4/DDR ownership and physical cache coherence remain unproved.

Direct MODE1-to-memory forms were rejected for this implementation after their
assembled encoding failed the focused execution check. The entry uses BIT TST,
BIT CLR and a SISD TF condition instead; no assembler or instruction-core change
is part of this private diagnostic.
