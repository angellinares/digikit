# Private stereo-frame fetch exclusion experiment

**[D]** A private assembled fixture and 59 named model checks establish a smaller
reader exclusion scope for one pending maskable writer. They leave existing SDK block kernels,
public ABI, current whole-block exclusion tests and production runtime unchanged.
This is not a shipping IRQ policy, a pool reservation or hardware acceptance.

## Reader and ownership boundary

Each of 32 stereo frames uses one masked scope for cached-generation admission,
epoch check, count acquisition, cursor/scalar copies and both channels' current
and clamped-neighbor packed words. The reader releases its count after the last
backing read, restores the exact incoming MODE1, then performs signed PCM16
conversion/interpolation from private copies, stores both channels and advances
the shared cursor once using `DK_SAMPLE_CURSOR_STEP`. Denial zeros every remaining
frame and does not relatch an epoch. Empty/completed/bounds cases read no PCM.

No-writer output and final cursor match the existing stereo kernel bit for bit
for forward, reverse, zero, fractional and extreme signed rates, terminal clamping,
empty/outside/completed states and same-plane sources. Computation order and
float32 model behavior are retained. The fixture uses scalar MODE1 `0x9000` or
incoming-disabled `0x8000`; it does not establish 40-bit arithmetic correctness.

**[O]** Cursor, output, save frame and tap scratch remain owned for the entire
callback, even while the backing count is zero. The test writer only closes
admission and reuses backing; assertions prohibit its stores to callback-private
storage. Backing drain alone does not authorize voice/module storage reuse or
prove callback quiescence. A production lifetime/serialization contract remains
necessary. No successor is published by this experiment.

## Actual DDR aliases and writer footprints

The reader uses NW32 DDR addresses in `[0x10000000,0x18000000)` mapping to byte
addresses `0x80000000+4*(w-0x10000000)`, through the existing addressing helpers.
This is explicitly different from `4*w`. Packed planes begin at physical byte
`0x8422c000` and `0x8422e000`; both descriptor bases may also select the same plane.
Each plane contains 65 PCM16 frames in 33 full words, so its owned footprint
includes the unused terminal high halfword.

The page writer affects exactly `[B+4096*p,B+4096*p+4096)`, with
B=`0x8422b9c8` and unsigned stock page bound p < `0x19200` (hexadecimal).
It is a private SISD 1,024-word fill of that actual footprint, not execution of the
stock page-copy instruction path. Page 0 overlaps only the left plane; page 2
only the distinct right plane. Any overlap with either packed-plane footprint
closes the whole binding before an exact-zero, fault-free drain and first store.
Nonwrapping DDR extents, stock page geometry, alignment and permitted lengths
are checked; rejected operands do not close admission or reach the guard helper.

The guard writer calls original DT2 1.17 helper `0x1c400f`, loaded at runtime from
section 7 with SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`.
For R4=start byte offset and R8=length byte offset, it reads ten bytes from
S=B+R4 and writes five two-byte values to S+R8+{0,2,4,6,8}. Source/destination
extents must be valid, aligned and nonoverlapping. The focused helper check
verifies exact writes, five load PCs (the watch layer records their ten individual
read bytes), frame/context preservation including I5, and matching physical aliases.
The guard starts at plane+130, overwriting the unused high half of the terminal
packed word that the clamped reader actually loads; its live low half is unchanged.
No original instruction bytes are embedded in the fixture or test.

**[C]** The preliminary `(SW)` DAG-store source forms were accepted but emitted
four-byte writes by the pinned assembler wrapper. This was an unsupported source
form in that route, not evidence of an emulator semantics bug. The original guard
helper resolves this experiment's exact-width requirement without an assembler
or core change. Byte-store source forms were not probed. A separate source-form
support/failure-reporting follow-up remains open.

## Complete arrivals and exact equivalence classes

For page and terminal-guard traces the test enumerates every instruction boundary,
including pending-before-entry and after-return arrivals: 5,173 and 5,205 boundaries,
respectively. It maps each arrival to its first model-eligible enabled boundary.
Distinct source planes test each one-plane writer; same-plane sources have one
physical footprint. These six configurations account for 31,134 arrivals.

The resulting deliveries form 33 exact classes per configuration, indexed by the
number of fully prefetched stereo frames. This is not sampled coverage: the full
trace checks that every descriptor/backing read is masked with count one and
every enabled boundary has count zero. Between fetches, only callback-owned
copies drive computation/store/cursor work. The writer mutates backing and close
controls only, uses a separate architectural stack and restores the entire saved
architectural context. Moving its delivery within one such interval therefore
commutes with that private work; the next admission denies. A prefetched but not
yet stored frame still finishes from old copies.

Each of the 198 representative executions checks equal old stereo prefixes then
zeros, cursor advancement by exactly that prefix and no torn frame. Resumed
callbacks are watched for any descriptor/backing load, including discarded loads;
stale callbacks after CLOSE read neither. The deliberate split-left/right lock
control tears the first stereo frame on an actual right-page overwrite despite
count zero. Count/fault, wrapping/sign-crossing source or destination, overlapping
source/destination, invalid page/alignment/length and exact nonoverlap boundaries
have focused controls. Rejected writer operands are watched for reads and writes
across DDR and their computed source/destination ranges; a helper-entry breakpoint
also proves no rejected source call occurs.

## Instruction observations and limits

| Reader fixture | Total model instructions | Largest masked span |
| --- | ---: | ---: |
| Per-frame, advancing/page scenario | 5,172 | 80 |
| Per-frame, terminal-clamped/guard scenario | 5,204 | 81 |
| Per-frame, incoming mask disabled | 5,172 | 5,172 |
| Existing whole-block represented-span reference | 3,485 | 3,462 |

These are measured instruction scopes, not cycle, IRQ-latency, speed or hardware
bounds. The bodies differ. Address-generation dependencies may stall on hardware;
the fixture also obeys the repository's four-intervening-instruction return-index
scheduling contract, checked from decoded instructions. The disabled incoming
mask stays disabled for the entire callback, so every pending arrival remains
pending until the caller later enables delivery.

The model exposes IRPTEN directly and preserves its existing bank pipeline.
The interleaving supplies one serialized, pending maskable writer, not physical
interrupt dispatch, DMA/NMI/cache exclusion, multiple producers or asynchronous
publication. Production geometry validation, complete writers, cached ownership,
callback lifetime and backing allocation remain open.

Sources: `tests/fixtures/dsp/sample-stereo-frame-fetch.s` and
`tests/patchkit/test_dsp_sample_stereo_frame_fetch.py`. Exact commands, immutable
tool identity, hashes, metrics and guard events remain under ignored
`out/runs/stereo-frame-fetch/`. The current owner check passes 59 tests with no skips;
scoped Ruff, formatting and reference checks accompany this checkpoint.
The original proof's staged fast checks passed; deferred merge-gate coverage
remains separate. For extraction, fast preflight passed Ruff/format/layer checks
but initially failed its path catalog on the untracked new header. After staging
that header, the scoped path check passes; a fresh full-wrapper pass is not claimed.


## Reusable raw-fetch extraction

**[D]** `sdk/dsp/sample_pcm16_fetch.inc` now supplies only
`DK_SAMPLE_PCM16_FETCH_WORDS(BASE, INDEX_A, INDEX_B, WORD_A, WORD_B, ADDR, DAG)`.
It reads two complete containing words in index order in eight instructions. Its
caller validates nonnegative indices/clamping and valid, nonwrapping word addresses
within one address space, supplies a linear DAG with PEYEN clear, and protects
both words across both reads. BASE/indices are preserved; output/address/DAG and
integer flags are clobbered, with only the two index operands allowed to alias.
There is no memory write, storage, label, unpacking, interpolation, validation,
mask or resource/output ABI. Raw normal-word addressing is the SHARC backend
implementation behind the SDK primitive. See
[the SDK contract and example](../../sdk/dsp/README.md).

Only this private fixture's two fetch pairs are refactored to use it. Both channels
remain inside the same admitted protection scope, retaining fetch order, private
copies and arithmetic. The original 58 checks and the new domain-edge case pass together (59 total),
with identical instruction metrics. A focused shared-index-register case reads the final valid
DDR word twice, retaining its raw unused terminal half, preserving inputs and
performing no stores; repeated inclusion/expansion is supported. The new primitive
is reusable; exclusion, generation and callback-storage lifetime policy remain
private experimental responsibilities. Existing mono/stereo kernels are untouched.

## Callback-private lifetime audit

**[D]** Both adapters run new-note state clear → `note_on` → `render` →
`_dv_rback` sequentially (DN2 source 451..483, DT2 source 1238..1296). Returning
from the module is distinct from adapter completion: the output guard still uses
output afterward. Sources are
`modules/core/adapters/dsp-voice/src/dsp/dsp-voice-dn2.s` and
`modules/core/adapters/dsp-voice/src/dsp/dsp-voice.s`. Save/context/input/parameter
scratch is shared DM; saving starts before validation. DN2 MODE1 pinning at
source 216/217 leaves IRPTEN/NESTM unchanged; neither adapter adds a recursion guard.

**[D]** The DN2 1.12 path is AudioTask `0x1c9fe7` → dispatch `0x1c9d6b` →
command 3 call `0x1c9fbc` → frame builder `0x1c2712` → call `0x1c3044` →
engine `0x1c8ef1` → call `0x1c9459` → hooked entry `0xb8251b`.
The slot loop decrements R15 at `0x1c9460` and advances at `0x1c9461` after
return: one uninterrupted invocation serializes its calls only. Seventeen targeted
instruction heads were checked once against original bytes; this remains [D].

**[C]** Database caller `0xb82658` jumps to interior `0xb82570`, bypassing the
patched entry `0xb8251b`; it does not establish a second hooked-entry path.
The SEC wrapper sets IRPTEN in saved MODE1 at `0x1c02bb`, restores MODE1 at
`0x1c03ea`, dispatches at `0x1c04b1` and clears IRPTEN at `0x1c04b7`. Known SPI/SPORT callbacks update
ring selection/notify the task; no direct hook path is recorded. **[O]** Indirect
callback replacement, other tasks/NMI and runtime non-reentrancy remain unresolved.
Backing count zero still does not authorize cursor, scratch or output reuse.

**[O]** Proposed next audit: retain callback-private cursor/scratch for the existing
boot-pinned module/voice-storage lifetime, untouched by backing replacement, and
install/reset bindings only at the existing audio callback boundary. Audit every
selection/row update that can reuse that per-voice storage before relying on this.
No mechanism is implemented or non-reentrancy fixed here. Selected-interpreter
PCDUMP has I6/I7/MODE1 and PC-stack depth, not stack contents or definitive nesting.
New trace/native runs are deferred; selection/row-publication reuse is the next audit.
Finite absence cannot prove scheduler-wide safety or authorize allocator reuse.

### Exceptional boot-load cleanup could make storage reusable

**[D]** At pre-fix revision `dce0a3d7`,
[dsp.rs](../../modules/core/runtime/src/rs/src/dsp.rs) `clear_rows` (1200–1201)
ignored a false `write_word_blocking` result and cleared `row_pub`;
`clear_fx_rows` (1219/1232) similarly discarded a failed PC-disable result before
clearing `fx_pub`. After one row published, a later row-transfer failure
(1468 → `fail`, 1296) could fail invalidation yet discard the publication marker.
`loader::undo_early` → `release` skipped the now-unmarked row, then unreserved
the DSP range (1244–1246). These line references describe that pre-fix revision.

`unreserve` cleared ownership/`mod_len` (1010–1039), making the extent eligible
for first-fit reuse (475–500). The
[loader](../../modules/core/runtime/src/rs/src/loader.rs) also called `fini`
(518 onward), released DSP storage and freed CF memory (527–529). This established
static erroneous reuse eligibility, not observed reuse or callback overlap.
Existing `PH_QUARANTINE` protected a pending transfer before `fail`, but did not
protect failed cleanup writes. Normal successful boot-pinned allocation stability
did not remove this exceptional path.

**[C] [D]** The runtime now retains publication markers on failed invalidation and
stops at the first unsuccessful write. Failed boot teardown closes CF slots,
waits for entered calls, then requires successful DSP invalidation before `fini`,
registry removal or storage release. An uncertain result enters sticky internal
`PH_RETAINED`: pending/queued reservations and transfer sources remain owned until
reboot; later steps, cancellation, reservation and word/image starts cannot reuse
them. This is distinct from pending-transfer `PH_QUARANTINE`. The original boot
failure is recorded once; teardown is not retried with the shared word buffer.

Ten host tests in
[cleanup_tests.rs](../../modules/core/runtime/src/rs/src/cleanup_tests.rs) execute
actual loader/DSP cleanup, publication flags, owner/range allocator and queue code.
Refusal, transport error, missing ACK, partial FX cleanup, absent FX info and CF
entered timeout retain storage; confirmed and prepublication cleanup reclaim it.
The host seam substitutes only firmware/link/info/IRQ and CF pointer side effects
(fini, registry removal, arena free/table inspection), not the cleanup algorithm.
The ColdFire object builds, re-decodes and passes its source-current check.
**[D]** Coordinator-owned normal-load acceptance passes on DT2 1.17 and DN2 1.12
(two cases, 235.08 s). It covers upload/persist/select/reboot, `MOD_DSP0`, nonzero
DSP extent and voice row/type, with no fault. It does not test rendered-audio
correctness or native failure injection. Commands/hashes are in ignored
`out/runs/dsp-cleanup-retention/commands.json`.
**[O]** Physical ACK/block-tail/callback ordering remains separate: host
state-machine tests do not establish DSP quiescence or fix adapter non-reentrancy.
The nested crate tests still need normal gate routing; no production backing
policy is selected.
