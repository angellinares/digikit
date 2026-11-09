# Stereo machine output and channel history

2026-10-30. DT2 OS 1.17 / DN2 OS 1.12. **[D]** source inspection and bounded
original-byte audits; **[C]** corrected interpretation; **[O]** integration open.
See the [sampler plan](../plans/dsp-sdk-and-sampler.md).

## DN2: stereo as two mono voice slots (2026-11-01)

**[D]** The DN2 note-on allocator `0x4002a6a2` (caller frame builder `0x40025e36`)
works on 16-bit slot masks. The per-track bitmap is `0x40288444 + track*0x103*4`.
Slot records have stride 0x14 from `0x40288304`. The steal mode byte
`_DAT_4059da30` is 0 (oldest), 1 (lower-priority tracks) or 2 (per-note
bitmaps). The allocator already makes N slots per note from `0x4059da00 + track`
(meaning **[O]**). A machine cannot request a second slot, and an adapter cannot
pair slots.

Pan is per track, not per slot. The final stage `0x1c9aea..0x1c9b30` applies the
owning track's two gain streams to each mono slot (planes `0x257ee8 + 0x100*v`,
B = A + `0x80`). Two slots of one track land at the same pan position.

Hard-panning would need a ColdFire allocator patch (pairing, paired steal) and a
SHARC gain-stage patch. It also halves polyphony (16 mono voices to 8 stereo).
Conclusion: this route is rejected. The shadow right-channel stages
(`core/dsp-stereo-proof`) are the DN2 stereo route.

## Current SDK boundary

**[D]** Machine contract 1.1 has one 32-float output pointer in `dk_voice_in`.
The DT2 adapter renders the even voice and explicitly copies its output into
the paired odd voice at `_dv_odd` in
`modules/core/adapters/dsp-voice/src/dsp/dsp-voice.s`. DN2's adapter passes one
32-float slot buffer to the module before the stock mono processing chain.
The separate FX contract already has two 32-sample planes; that does not make
machine output stereo. A second field in the SDK input alone is insufficient.

## DN2 channel split: correction

**[C]** The historical description of `0x1c9aea` as 32-to-64 temporal upsampling
is contradicted by current original bytes. This includes the legacy description
in the DN2 adapter's source comment. The code creates **two 32-sample planes**
from one 32-sample mono block, multiplying each input by separate gain streams.
It does not double the temporal sample count.

**[D]** DN2 1.12 SYX SHA-256:
`7e5b8a733d4ab1fdf7e5d22757f7d8eddaa3cbf71d0858874a791844c02ceb35`.
Section 7 is 836,956 bytes, SHA-256
`336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`.
The auditor freshly extracted these bytes and checked the listed instructions.

Initialization at `0x1c14e1..0x1c1575` builds pointer table `0x258ee8`:
channel A for slot v is `0x257ee8 + 0x100*v`; channel B is A+`0x80`.
The table is installed through `0x1c2848/4b` and read by the engine at
`0x1c9add`. At `0x1c9af5` one input is loaded; `0x1c9af6/aff` multiply that
same sample by two coefficients and `0x1c9af9/0x1c9b0c` store the pair.
The loop repeats for the next input at `0x1c9b1b/21/2d`.

The gain sources include complementary tables `0x8052d790/0x8052d990`, with
endpoints `(1,0)` and `(0,1)` and midpoint `(0.70710683,0.70710683)`.
This supports pan-channel interpretation; physical connector ordering was not
separately traced. Downstream `0x1c1738` and `0x1c1f1e` read paired planes
32 words apart and process 32 sample pairs. Caller order is engine at
`0x1c3044`, then those consumers at `0x1c3054/5d`.

Initialization writes 48000.0 to `0x257e5c` at `0x1c1487` and block count 32
to `0x257e6c`. This establishes configuration and temporal-count preservation,
not a physical clock measurement. There is no 96 kHz conversion in this loop.

## Mono processing before that split

**[D]** Hook `0xb8251b`, called at `0x1c9459`, receives one slot buffer as
both R8 and R12. Buffers start at `0x804acf90`, stride `0x80` bytes. Scalar
gain and later stages process that buffer in place before the split. Two finite
stage audits establish independent audio histories without internal control
advancement:

| Stage / call | State per slot v | Coefficients read | Audio history written |
| --- | --- | --- | --- |
| `0xb80c39 → 0xb809f2`, call `0x1c94fd` | `0x24f320 + 0x70*v` | Words 21,22,23 | Words 24,25 |
| `0xb81368`, conditional call `0x1c959f` | `0x2533e0 + 0x20*v` | Words 2,3,4,5 | Words 6,7 |

Both process the same input/output buffer, with a normally configured count of
32 and sixteen slots. In these finite bodies, other accesses are sample I/O and
stack traffic; there are no global/RNG writes or coefficient/ramp updates.

The first stage is `y = a*x + b*x_previous - c*y_previous`, then updates the
two histories. The second stage, with coefficients `a,d,b,c` in words 2..5, is:

```text
Unew = a*U - d*x
Ynew = c*Unew - c*U + b*Y
```

The second stage loads coefficients once per block. Its separate float products
and subtraction order matter. Under fixed coefficients, its real-arithmetic
transfer function can be expressed as the SDK biquad with
`b0=-c*d, b1=c*d, b2=0, a1=-(a+b), a2=a*b`. That is **not** established as a
bit-identical replacement: a float32 example already differs by one ULP, and
state conversion depends on coefficients. Do not collapse the recurrence or
reuse one channel's history for both channels.

**[O]** These stages can share captured coefficients while retaining independent
left/right histories. This does not establish the same property for the entire
chain or exclude external coefficient changes. Remaining work is an ordered
stage/state inventory, shared control evolution once per block, independent
channel processing, and the existing pan/send path. A late stereo insertion
alone would bypass earlier mono processing and is not the selected solution.

No module ABI change, firmware patch, emulator run or hardware test is claimed
by this finding.

## Finite post-source inventory

**[D]** A further original-byte pass checked 722 aligned instructions in
`0x1c9448..0x1c9b33` and 99 local continuation/dispatch instructions in
`0x1c9b73..0x1c9c5d`. There are ten processing families. The two recurrences above were audited first;
the eight additional footprints are now classified below. Their placements are:

| Callee | State base + byte stride per slot | Placement |
| --- | --- | --- |
| `0x1cdb56` | `0x24fa20 + 0x60*v` | Conditional early or later scaled placement |
| `0xb8265b` | `0x250160 + 0x14*v` | Conditional early or late placement |
| `0xb82342` | `0x2505a0 + 0xcc*v` | Dispatch case 1 |
| `0xb81a16` | `0x251260 + 0x12c*v` | Dispatch case 2 |
| `0xb80f2e` | `0x252520 + 0xec*v` | Dispatch case 3 |
| `0xb806f5` | `0x2535e0 + 0x48*v` | Dispatch cases 4 and 6; existing ring bounds in finding 106 |
| `0xb81526` | `0x253a60 + 0xd4*v` | Dispatch case 5, scratch output then blend |
| `0xb80345` | `0x2547a0 + 0x2c*v` | After selected processing stages |

The seven-entry dispatch table at `0x8052dbc0` has case 0 as skip. Other cases
process the mono buffer in place except case 5, whose scratch destination comes
from `DM(0x254aa0)`; its continuation blends scratch and the original buffer.
Conditional placements use tables at `0x254ab4`, `0x254aa4`, `0x254ac4`.
Their full flag domains are not established, so early/late exclusivity is not
assumed for arbitrary values.

The ordered path is constant gain, conditional `0x1cdb56`, audited first-order
stage, conditional `0xb8265b`, conditional audited U/Y stage, selected dispatch,
late conditional placements, `0xb80345`, gain ramp, conditional fade, metadata
cleanup, then the two-channel split. Most control/coefficient generation is
before the source hook at `0x1c9045..93c6`. Some physically later code rejoins
that earlier control flow; it must not be counted as extra post-source stages.
The inline cleanup mutates shared flags and must not simply run twice.

**[O]** Live routing-table invariants, external state writers and reset/lifetime
coverage remain open. The local classifications below do not authorize duplicating
the whole chain's execution.


## Remaining eight state footprints

**[D]** Original DN2 1.12 section-7 byte audits classify all ten local bodies.
Offsets below are 32-bit words in the passed record; the placement strides above
are bytes. These are finite local read/write audits, not whole-engine ownership.

| Stage | Persistent reads | Persistent writes and interpretation |
| --- | --- | --- |
| `0x1cdb56` | 0,1,2,5,12 | 4,5,6 and 11,12,13: two shared old/current/step control groups; no audio history |
| `0xb8265b` | 0..4 | 1,2 held audio; 3,4 phase counters. Share captured timing, retain independent held samples |
| `0xb82342` | 3,4,5,6,7,12,14,26,28,33,35,40,42 | 3,4 recursive histories; 48,49,50 audio-dependent snapshots. Keep all five channel-local pending external-consumer audit |
| `0xb81a16` | 10,11; 12+7k and 14+7k, k=0..8 | 10,11 histories only; coefficient ramps evolve locally |
| `0xb80f2e` | 0,1,2,3,4,5,9,10,11,12,14,18,25,32,39,46,53 | Audio histories 5,9,10,11,12; shared block preparation writes {17,18,19}+7k, k=0..5 |
| `0xb806f5` | 0,2,4,10,13,14,15,16,17 | Audio history 0 and external ring; control old/current/step 3,4,5; read coordinate 13, write cursor 14, fill status 17 |
| `0xb81526` | 9,10; 11+7k and 13+7k, k=0..5 | 9,10 histories only; coefficient ramps evolve locally |
| `0xb80345` | 0..7,9,10 | 7 phase, 9 counter, 10 gain marker; audio-independent control evolution |

The nonlinear `0x1cdb56` uses captured control ramps and memoryless clipping/
shaping. Replaying it or `0xb80f2e` on already advanced shared state changes
its ramp. For example, `0xb80f79` copies word 4 to prior-value word 46; another
invocation immediately computes zero difference there. Shadow control changes
must survive repeated occurrences within the right-channel replay, then remain
separate from authoritative control state.

`0xb8265b` reads table `0x26e9c8`; its active path is fixed at 32 samples.
`0xb80f2e` reads `0x26b3a8` with adjacent dynamic indices; their full domain is
not audited. `0xb80345` reads tables `0x26a570/0x26a770/0x26a970`; upstream
index bounds remain open. Its unity paths assume in-place input/output. Its
stack arguments are control, byte control, then sample count, unlike a simple
single-count wrapper. No new mutable globals or RNG effects were found in
these bounded bodies apart from the external ring below.

`0xb806f5` word 16 points at `0x804ad790 + 0x8000*v`, established by
`0x1c8c05/0b/0d`. Constructor `0xb80575 → 0xb80546` clears each 32 KiB ring;
reset `0xb805d8` clears history/cursor/status without clearing the backing.
**A copied record retaining that pointer would share audio history.** Ownership
of an independent second-channel ring is not established. Initial coordinate
bounds and DAG/SIMD entry assumptions remain open. Case-5 scratch is
`0x804ace90`, installed through `DM(0x254aa0)` by `0x1c8c02/08`; consume or
retain each result before reusing it, including the continuation blend.

Arithmetic helper `0x1c06ba` has no memory accesses in its actual body. Its
returns at `0x1c06fa` and `0x1c0701` include delayed-return forms; later copy
code printed by the database is outside the helper. Its temporary MODE1
rounding-bit change is restored on the bounded normal path. The truncation
helper `0x1c07cc` likewise restores its temporary mode. These observations do
not cover exceptional floating-point inputs or arbitrary live contexts.

## Bounded shadow replay proof

**[D]** `tests/fixtures/dsp/stereo-shadow-replay.s` exercises a synthetic ordered
subset: `0x1cdb56 → 0xb80c39 → 0xb81368 → 0x1cdb56`. This is deliberately
not identified as a stock default route. Original firmware is loaded only at
runtime, hash-bound to the section above; no firmware bytes are in the fixture.

The assembled preparation copies initial shared controls and coefficients before
left processing, preserving independent right histories. Right control mutations
persist between repeated stages but are never published to the left record.
Three changing-control blocks with unequal channel inputs match two isolated mono
reference executions exactly within the configured numerical model. Explicitly
resetting both histories and controls reproduces block zero. Store checks bound
writes to declared state, audio and stack, with exact stage-write counts.

Six focused tests pass without skips. Negative controls detect copying controls
after left processing, clearing right histories between blocks, and restoring
initial controls between repeated occurrences. The harness also checks original
image preservation, linked-code overlay priority and per-image decode-cache
isolation. Independent source review found no issues.

**[O]** The combined test runs on the default core. RECIPS and RSQRTS use the
measured seed tables (finding 102), so the division helpers are exact. A separate
three-block recurrence-only test covers the recurrences alone. This establishes
model state-separation equivalence, complete
routing, pan/send integration, natural reset/retrigger/steal coverage, external
writer serialization, throughput or production stereo ABI.

**[D]** A reset candidate is `0xb8135a` (zeros words 6/7). The call at
`0x1c96aa` lies in a physically later pre-hook branch: `0x1c9343 → 0x1c9642`,
then `0x1c970e → 0x1c9346`, before source hook `0x1c9459`. Numerical address
order is not execution order.

An independent original-byte audit resolves the reset argument to
`0x2533e0 + R10`: the delayed call at `0x1c96aa` sets `R4=R5+R10` in its
`0x1c96ad` delay slot, overriding the earlier `R4=M9`. Caller `0x1c303e`
supplies engine base `0x241298`, propagated to I9 by `0x1c8f18/1d`.
`0x1c928b/93/96` computes R5 as that base plus `0x12148`. Complete bounded
callee audits (`0xb813e8`, `0xb8028c`, `0xb81d6d`, `0xb818c7`, `0xb81c8d`,
`0xb811f1`, `0xb82440`) preserve R5/R10 along this path. `0x1c93aa/ae`
increments R10 by `0x20`, matching the processing-record stride. Exact slot
ordinal mapping and the event-level meaning of the SV branch remain unaudited.

**[C]** An intermediate database-only interpretation of engine pointer
`0x257e6c` as this reset argument is contradicted by the delayed argument
setup. An apparent instruction at `0x1c96a9` is actually the final parcel of
`0x1c96a7`; a frame-slot propagation inference also used a value from before
its reload. Neither interpretation may be used to place a reset hook.

This establishes one concrete per-slot reset path, not complete initialization,
retrigger/steal/filter-change coverage or an implemented shadow reset hook.
The audit used original bytes with the project decoder, not a hardware trace.


## Additional filter replay proofs

**[D]** The replay fixture now covers the three isolated leaves `0xb82342`,
`0xb81a16` and `0xb81526` in
`tests/fixtures/dsp/stereo-filter-shadow-replay.s`. Each runs three unequal-channel
blocks with changing captured coefficients and independent nonzero histories.
The candidate matches two isolated mono sessions, keeps left state unchanged by
the right pass, and bounds stores to declared audio, audited mutable fields and
a 65-word stack window. Deliberately replacing right histories with left end-state
must change actual right audio, so silence or a bypass cannot satisfy the proof.
For `0xb82342`, all five audio-dependent fields, including snapshots 48..50, stay
private. These six new cases and the six existing replay cases pass without skips;
independent source review found no issues.

The new leaves use byte-space state/audio/stack pointers with explicit
`assume_nw32=True`, because their raw-byte MODIFY interiors are combined with
normal-word loads/stores. This context is
part of the proof, not an unqualified claim of arbitrary-mode equivalence. The
preparation fixture accesses equivalent normal-word aliases, while stock callees
receive byte pointers. No core semantics, public ABI or runtime code changed.

Six of the ten local stage families now have replay experiments. Case-5 scratch
and continuation blending, the remaining four families, complete routes, external
writers, reset integration and actual pan/send output remain outside these tests.
No hardware throughput conclusion follows from numerical leaf tests.


## Shared reset predicate and first-order reset

**[D]** A fresh original-byte audit closes another bounded lifecycle question.
`0x1c9336` loads byte `DM(I2-17)`; LEFTZ at `0x1c9338` and `IF SV` at
`0x1c933b` skip reset for zero. Otherwise `0x1c933e` loads `DM(I14+5)`;
LEFTZ at `0x1c9340` and `IF NOT SV` at `0x1c9343` select the reset sequence
for nonzero. Both inputs must therefore be nonzero. Public Programming Reference
printed p. 19-66 describes unary LEFTZ with SV set for zero input; the rendered
second operand R0 is unused. This is a zero test, not an arithmetic-overflow test.

The same sequence also resets the first-order histories. `0x1c966c/6f/71`
saves `I9+0xe088 = 0x24f320` in frame slot −3. `0x1c9681` loads R14 from
frame slot −18; `0x1c96c7/c9/cb` reloads the base and calls `0xb80b93`
with base+R14. The complete reset body, including return delays, zeros words
24,25,26,27 at `0xb80b97/99/9b/9d`. Frame −18 starts at zero (`0x1c9035`)
and advances by `0x70` at `0x1c9370/7b/86`, stored at `0x1c93a0`.
Complete bounded callees `0xb811f1`, `0xb82440`, `0xb8135a`, `0xb805d8`
with nested `0xb80e48`, and `0xb827d8` preserve R14 between load and reset.

Local loop flow initializes R3/R10 to zero, increments R3, tests `R3>>4`, advances
R10 by `0x20`, and loops while SZ. This supports sixteen physical record ordinals
with U/Y offset `0x20*v` and first-order offset `0x70*v`. It does not establish
user-facing voice identity. Local arithmetic suggests guard byte `0x254ba4+v`
and word `0x2547b4+0x2c*v`; the former depends on correctly decoding the byte
post-modifier at `0x1c911a`, which the listing's suffix omits.

**[O]** Upstream indirect dispatches `0x1c90b0/0x1c91ad` and their complete
register-preservation graphs are not exhausted. Keep full guard-address/ordinal
propagation conditional on that gap. Neither guard is identified as note-on,
retrigger, steal or filter change. Next bounded question: their actual producers.
This is a concrete common reset path, not complete lifecycle coverage.


## Held samples and shared block preparation

**[D]** `tests/fixtures/dsp/stereo-prepared-shadow-replay.s` extends the numerical
replay experiment to `0xb8265b` and `0xb80f2e`. Six new cases plus the previous
twelve pass without skips. Both run unequal channels over three changing-control
blocks; right histories stay independent and the left record matches an isolated
reference after right execution. Negative controls copy already advanced controls
or replace right histories with left state and must change output samples.

Held-sample tests use active table indices 32,48,64 with original finite periods
between 6 and 26 samples. Every block includes both repeated output and refresh;
the disabled copy path cannot pass. The preparation-stage tests retain five audio
histories and copy shared input/control fields before the left pass. All dynamic
table-load PCs are watched over the full address space, so a load outside the
128-entry or 1,024-entry tables fails rather than disappearing from the watch.
Canonical store bounds include only permitted state, audio and stack locations.

Both stages run on the default core with the measured RECIPS seed table
(finding 102). Byte-space pointers and
32-bit normal-word accesses are explicit, as in the filter-leaf tests. These are
bounded captured-control cases, not complete parameter domains or live routes.
Eight of ten stage families now have replay tests; the ring-backed stage and
final gain stage, plus inline blend/pan/route/lifecycle integration, remain open.

## Reset guard producers: bounded follow-up

**[D]** Setter `0xb8028c` writes state word 5 at `0xb802f9` from stack argument
loaded at `0xb802d2`. Caller `0x1c92a5` pushes R15, loaded at `0x1c9285` from
`DM(I5-1)`; `0xb813e8` preserves R15 through its delayed restore. Visible frame
arithmetic supplies `0x2556dc + 0x234*v`. The frame builder writes that same
word at `0x1c2c14`, after arithmetically shifting the packed word read at
`0x1c2b00` right by 16 and then 8. Thus the guard receives a sign-extended high
byte from word `0x25c5ea + 0x92*v` (byte `0x25c5ed + 0x92*v` in the local
little-endian frame). Its event/parameter name is not established.

Helper `0x1c9c9d`, called at `0x1c31a7` with delayed arguments, writes M13
(normally zero) as a byte to `0x254ba4 + slot` at `0x1c9cc0`. It also writes
M13 to `0x254b94 + slot` and M14 to `0x254bb4 + slot`. The caller invokes it
when a signed-short frame value is nonzero. This is a concrete clear writer;
the writer that makes the guard nonzero remains unidentified. Empty targeted
reference results do not exclude other writers. Upstream indirect-preservation
and event-name caveats above still apply; no lifecycle hook is selected yet.


## Final gain replay

**[D]** Four more cases in `tests/fixtures/dsp/stereo-gain-shadow-replay.s`
exercise `0xb80345`: linear attack followed by exponential progression toward
0.5, and a second record with a 14-sample in-place unity hold followed by release
toward zero. Nine blocks include two uses of the actual entry reinitialization
control. All eleven words are captured before left processing; right output and
control state match an isolated mono reference, and right processing leaves the
left record unchanged. Copying advanced controls after left processing must
change the right output. No audio-history fields are introduced for this stage.

The full reachable body ends at `0xb80545`, beyond the database function boundary.
Its consumed-count calculation divides a byte-pointer difference by four, so the
proof uses byte-space audio pointers, explicit 32-bit normal-word access context
and in-place buffers. Table-load PCs are checked against 128-word table bounds;
state writes are restricted to words 7,9,10, plus audio and bounded stack stores.
The hold path may leave existing in-place audio untouched. The trajectories
avoid the phase-5 division helper.

The synthetic record begins with indices 7,1,7,4th-word float 0.5, then 7,1,0,5,
0,0,0. Constructor/setter provenance was separately inspected. Word 9 is explicitly
initialized: the constructor does not initialize it. The alternate record uses
word 6=1 and target word 3=0. The entry control rewrites phase/gain markers; this
is not a complete-record reset or a named note-on/retrigger event claim.

All 22 replay tests pass without skips. Independent source review found no issues;
the primary worktree received byte-identical tested files from the isolated gain
worktree. Nine of ten local stage families now have bounded replay experiments.
The ring-backed stage, complete ordered routing, scratch blend, pan/send integration,
external writer coverage and production lifecycle still remain open.


## Ring backing and source continuation replay

**[D]** The ring fixture `tests/fixtures/dsp/stereo-ring-shadow-replay.s`
executes the actual constructor at `0xb80575` at 48 kHz, checking all 8192
initialized words in each independent 32 KiB backing. Three blocks through
`0xb806f5` wrap the write position from 8190 with integer delay 32. Right audio,
state and backing match an isolated mono reference and leave the left backing
unchanged. Read and store bounds are checked; deliberately sharing the backing
fails the ring-store boundary check and changes left memory. Private record words
are 0,13,14,16,17; shared controls are captured before left execution. Word 17 has
byte-wide fill stores. This case uses the existing qualified reciprocal
approximation, avoiding the known floating endpoint at 8192. It does not establish
all delay values, lifecycle coverage or production allocation.

**[D]** The separate source continuation `0xb8251b` is outside the ten post-source
families. Original-image inspection found no persistent state stores, nested calls
or RNG in its reachable body. Its positive-control branch processes exactly 32
samples and uses companion SIMD registers as scratch. The fixture
`tests/fixtures/dsp/stereo-source-shadow-replay.s` exercises controls 0, 0.25, 0.5,
0.75 and 1, with unequal finite input in [-0.8,0.8], explicit byte/NW32 pointers,
linear address context and MODE1 initially zero. Both outputs match separate mono
references; bypass is exact, positive outputs change and remain finite, record
word 0 stays unchanged, and table/audio/store bounds hold. MODE1 returns to zero;
S0/S5/S9/S14 are treated as scratch, not promised preserved. Reciprocal
approximation is disabled. The positive controls cover depths 8,4,2,1; the depth-31
shift edge and full control domain are not covered.

All 29 integrated cases pass without skips. Independent source review of both
additions found no issues. All ten identified post-source families now have
bounded numerical examples, plus the source continuation. Complete ordered routing,
inline scratch blend, pan/send integration and production lifecycle remain open.


## Selected ordered-route replay

**[D]** `tests/fixtures/dsp/stereo-ordered-shadow-replay.s` composes the source
continuation, first-order filter, case-5 filter into private scratch, original
inline scratch blend, final gain and original output split. The blend runs from
`0x1c9c12` through its delayed final store at `0x1c9c44`, stopping at `0x1c98b9`;
the split runs from `0x1c9aea` and stops at `0x1c9b30`, before the outer-loop endpoint.
The synthetic route chooses unity scalar/later ramps and disables other optional
stages/fade. It does not establish that these routing flags occur together in
stock operation; shared cleanup remains outside the replay.

Four blocks compare all four intermediate buffers and both output planes from
each channel with independent mono references. Controls are captured before left
execution; first-order and case-5 histories remain private. Read/store bounds cover
state, stack, table, scratch and audio accesses. Gain traverses phases 2,2,1,1.
All values remain finite. Negative controls
catch shared histories, advanced gain state, scratch overwritten before blending,
and swapped or duplicated terminal planes. The scratch fault first verifies an
otherwise-correct filter result, then detects the numerical blend mismatch.

The proposed terminal pair takes plane A from the left input and plane B from the
right input, without summing cross-contributions or extra normalization. Both
private pairs are checked against mono references first. This is a candidate
balance mapping, not a finalized pan policy or public ABI. Stock DT2 stereo pan
behavior is being checked before adopting it. Send consumers, full engine entry
context, natural reset/retrigger/steal, all slots and production routing remain
outside this bounded proof. Independent source review found no issues.


## DT2 paired-input pan comparison

**[D]** Fresh DT2 1.17 section 7 (321,016 bytes, SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`)
shows independent scaling in the original pan fragment `0x1c711a..1c7155`.
This holds on the gate-1 path only; with gate 0 the pair is summed
([finding 112](112-dt2-pair-gate.md)).
Inputs are separate pointer-table entries 2t/2t+1 at `0x24ef2c`; destination
initialization at `0x1c1699/9c` with stride update `0x1c16d7` establishes entry
v at `0x252df8 + 0x80*v`. Each pair therefore has two distinct 32-float buffers.
The first sample is independently loaded/scaled/stored at `0x1c7124/25/2a` and
`0x1c7129/2a/34`; subsequent samples remain separate at `0x1c7140..1c7155`.
There is no cross-channel addition in this inspected fragment.

The 128-entry coefficient tables at `0x8055c440/0x8055c640` have endpoints
(1,0)/(0,1), with index 64 approximately (0.70710683,0.70710683). Each target
also includes its own level squared and factor approximately 0.35479999.
Control preparation clamps/interpolates separately and publishes start/target/
increment state before audio processing. The audio loop reads those streams;
conditional preparation can snap starts and zero increments. Consequently the
candidate DN2 diagonal left-A/right-B mapping is consistent with this stock DT2
boundary, without claiming identical complete processing or parameter policy.

This does not establish UI hard-pan mapping, physical connector identity or full
upstream/downstream routing. A zero coefficient removes its input here rather
than folding it into the opposite output. The existing module adapter's even-to-
odd copy is its current mono behavior, not evidence of native channel folding.

## Remaining local reset helpers and control dependencies

**[D]** Original DN2 1.12 bytes identify the remaining private-history resets.
Offsets in this table are words; record strides are bytes. Full helper bodies,
including return delays, were inspected under the usual SISD/linear/NW32 context.

| Audio family / record | Reset helper | Local engine calls | History behavior |
| --- | --- | --- | --- |
| `0xb82342`, `0x2505a0+0xcc*v` | `0xb81d6d..0xb81e1f` | `0x1c964b`, `0x1c9716` | Clears 3/4; preserves snapshots 48/49/50 |
| `0xb81a16`, `0x251260+0x12c*v` | `0xb81c8d..0xb81cc0` | `0x1c9684`, `0x1c9747` | Clears 10/11; also initializes coefficient groups |
| `0xb80f2e`, `0x252520+0xec*v` | `0xb811f1..0xb81278` | `0x1c9692`, `0x1c976f` | Clears 5..12; also initializes coefficient groups |
| `0xb8265b`, `0x250160+0x14*v` | `0xb82440..0xb82453` | `0x1c91c5`, `0x1c96a3` | Clears all words 0..4, including return-delay store at `0xb82450` |

Constructor paths also invoke those helpers: `0x1c8d2b → 0xb81fa4`,
`0x1c8d39 → 0xb81cc0`, `0x1c8d51 → 0xb81278`, and direct initialization at
`0x1c8d07 → 0xb82440`. The latter helper also initializes the separate source
record at `0x1c8d10`; helper address alone cannot identify the held-sample family.
The common reset branch alone also misses the other local calls in the table.
Delayed R4 arguments and the previously qualified upstream indirect preservation
must be respected; these addresses do not establish named lifecycle events.

For `0xb81d6d`, record writes are
{3,4,12,13,14,17,19,20,21,24,26,27,28,31,33,34,35,38,40,41,42,45}.
Only {3,4,14,21,28,35,42} are zeroed; others copy or compute controls.
`0xb81c8d` writes 10/11 and {12,13,14,17}+7k for k=0..8.
`0xb811f1` writes 5..12 and {17,18,19,22}+7k for k=0..5.
A generic full-record clear would not reproduce any of the first three helpers.

**[D]** The immediate case-1 coefficient preparation path
`0x1c9253 → 0xb82096 → 0xb81e1f` does not consume audio histories 3/4 or snapshots
48/49/50. The outer setter has no record loads; the nested setter reads only
13/27/34/41 and updates coefficient ramps. Inspected adjacent constructors and
setters also reveal no snapshot consumer or initializer. This finite result
supports the current local separation; it does not prove globally unused state,
exclude retained pointers/bulk copies, or authorize ignoring external consumers.

## AudioTask context and snapshot-stability gap

**[D]** The immediate caller chain is AudioTask entry `0x1c9fe7`, dispatch
`0x1c9d6b`, command-3 path `0x1c9f0f/0x1c9fbc`, frame builder `0x1c2712`, then
engine `0x1c8ef1` called at `0x1c3044`. The builder copies 672 words at
`0x1c302d..33`; engine preparation begins at `0x1c9045`. Source processing is
inside the sixteen-slot loop at `0x1c944e/59/60/61`.

No explicit IRPTEN clear or IMASK write was found in those inspected builder/
engine bodies; their explicit MODE1 changes concern SIMD only. The SEC wrapper
permits handler execution with interrupts enabled. SPI input DMA and selector
updates, SPORT callbacks and task notification remain concrete asynchronous
avenues. This bounded trace does not establish that they write processing records,
but AudioTask naming alone cannot prove serialization. Callee masks, scheduler
priorities, indirect writers, DMA and NMI remain outside the local proof.

Capture-before-left remains necessary. A short interrupt mask would only address
identified maskable CPU writers, not establish whole-route DMA/NMI stability.
The production snapshot/commit boundary is therefore still unresolved; numerical
replays with deliberately stable controls do not close that gap.

## Actual-helper reset replay

**[D]** `tests/fixtures/dsp/stereo-reset-shadow-replay.s` executes the four reset
helpers above on independent records, then compares reset state and subsequent
rendering with isolated mono references. Nine added cases cover correct mirroring,
a missed reset in each family, and blanket clearing of snapshots 48..50 in the
first family. Every missed reset changes subsequent audio. Blanket clearing is
caught at the reset boundary; the next audio call legitimately overwrites those
snapshots and could otherwise hide the mistake.

Right execution leaves left state unchanged. Record and stack accesses are bounded;
the first helper's 128-entry coefficient-table reads are bounded too. The held
helper is explicitly bound to its held-sample record, and its return-delay store
is included. Reset calls do not divide; the prepared-stage rendering divides with the
measured seed table (finding 102). Independent
source review found no issues. These tests establish finite helper behavior, not
natural event coverage, all asynchronous writers or an installed reset hook.

## Bounded callback and DMA follow-up

**[D]** Recorded AudioTask paths reach the named setters/resets through
`0x1c9e76 → 0x1c2712 → 0x1c8ef1`. Other direct callers sometimes pass different
object-relative subrecords; their identity with the engine records is not proved.
The case-1 argument preparation starts at `0x1c9253`; the actual call to
`0xb82096` is `0x1c9268` (then `0xb82124 → 0xb81e1f`).

The SEC handler table is populated through `0xb8ce24 → 0x1c066c`, with the handler
stored at `0x2404b0[index]`. SPI2 registration supplies its driver handlers. Its
user callback is `0x1ca04a`, installed by `0x1ca4b8 → 0x1cb7d2`; dispatches at
`0x1cc265/0x1cc575` read that binding and zero context. This callback changes the
input-ring selector `0x2c0450`. SPORT initialization binds `0x1ca136` through
`0x1ca2b2/b5 → 0x1ccba3`; dispatch `0x1cd813` supplies event 0x100, leading to
AudioTask notification. No concrete processing-setter path was found through
these bound callbacks. Registration remains mutable; this is not global exclusion.

Main SPI2 RX descriptors cover `[0x2c29d0,0x2c348c)` and
`[0x2c39d0,0x2c448c)`, disjoint from the processing records. Auxiliary SPI setup
has known roots but unresolved active length/activation. Generic LP0 submit
`0x1caaa6` accepts caller descriptors: word 1 becomes DMA_ADDRSTART after alias
normalization, and words 3/4 become count/stride. The normalizer accepts private
L1 addresses, including the processing-record range. That is an accepted
configuration possibility, not evidence of an armed overlapping transfer.

A bounded direct-producer and original-byte pointer search identified no stock
submission into that LP0 wrapper. Computed calls/constructed pointers remain open;
an empty recorded caller set does not prove dead code. The concrete normal
AudioTask input path instead selects SPI input via `0x1c9d96 → 0x1ca020`.
The sole repository proof adapter's LP0 descriptor always uses its boot-derived
L2 receive pointer, 4,100 bytes, stride 4, and no next descriptor; rearm rebuilds
that same destination. Its later DDR copy is a CPU copy.

LP0 STOP still loads a caller-supplied callback/context installed by `0x1cab60`;
no concrete stock application binding was established. The missing evidence is
an actual submitting/registering callsite and its descriptor owner, not another
negative cross-reference search. These results support a bounded native prototype
without asserting universal DMA/NMI exclusion or changing interrupt policy.


## Initial private first-order native identity checkpoint

**[D]** The optional DN2 1.12 `core/dsp-stereo-proof` adapter retargets only
engine render `0x1c94fd → 0xb80c39` and local reset `0x1c96cb → 0xb80b93`.
Other callers remain unchanged. Sixteen linker-owned private records/audio buffers
consume 3,992 L2 bytes, with no DDR reservation or default activation. Exact slot
validation uses R4=`0x24f320+0x70*v`, v=0..15; unsupported contexts tail-call stock.
The original left call/output remain authoritative. First use clones the record;
each block copies 32 input floats and coefficient words 21..23 before left, then
runs stock right with private histories. Reset mirrors words 24..27 only after
initialization. This observes identity; it does not replace audible output.

The 23 owner model cases pass, including invalid-slot/count and unexpected-SIMD
passthrough, private-history corruption and NaN-history detection. The SIMD case
stops at stock entry and does not assert that stock supports SIMD. Scalar status
restoration includes post-left ASTATX/STKYX; this is not a production floating-point
exception policy. The wrapper avoids writing untouched callee-saved data registers
rather than claiming that a 32-bit model proves their 40-bit precision. The largest
identity fixture took 2,628 instructions; this is an observed fixture maximum.

The bounded native acceptance passed: 2,400 render/shadow calls, one reset mirror,
zero mismatches/fallbacks, 2,400 blocks with nonidentity coefficients and 76,799
nonzero input samples. Original and patched captures each contain 4,800 stereo
frames (100 ms), with identical whole-WAV SHA-256
`6be721f8b1649c0a037bd8bd1a0998ff5847ed80de31b1907f98b01604555b0c`
and zero alignment shift. Both real channels are finite and nonzero. Forced
snapshot reload, successful snapshot/audio activation and ready-input hashes are
asserted. An independent artifact review confirmed the identical WAV hashes.
The daemon's legacy core profile label remains `dn2-1.11`; CF version 1.12 and
original base/ready hashes identify the actual input, not a new generated core.
Final regression TypeScript and scoped oxlint checks pass.

The AudioTask allocation is 4,000 bytes, correcting the earlier stack-size claim
in [finding 83](83-sample-retirement-design.md). In this native scenario the lowest
entry frame minus the model-exercised 172-byte store depth leaves 3,132 bytes above
the allocation base. This is not a measured native store watermark, a universal
stack bound or IRQ/scheduler headroom proof.

Exact commands, binary/source hashes and logs remain under ignored
`out/runs/dsp-stereo-proof/commands.json` and `native-acceptance.log`; the result is
`out/runs/dsp-stereo-proof/native/7388abd1-c4a3-4fdf-8199-edc11d2bfc16/result.json`.
Original firmware and captured audio remain ignored runtime inputs/artifacts.
This proves one exercised first-order caller/reset scenario. Full-route lifecycle,
coherent capture/commit, cleanup, asynchronous writer exclusion, SDK ABI,
production pool ownership, performance and hardware behavior remain open.


## Private first-order polarity isolation checkpoint

**[D]** The current observer adds an opt-in pre-first-use polarity mode while
identity remains the default. It XORs the sign bit of copied right inputs and
initial live histories 24/25; coefficients 21..23 and stock operation order stay
unchanged. It compares inverse audio and live histories with signed-zero equality.
Invalid/changed mode latches refusal until reload. Polarity mode also latches
refusal on nonfinite coefficients, input, live histories or output; identity keeps
its previous exceptional-value comparison behavior. Original left state, output
and context remain authoritative, with unchanged SIMD/fallback guards. This is a
finite, same-numeric-mode metamorphic relation, not an independent correctness
oracle, a 40-bit model proof or a production exceptional-value policy.

All 39 model cases pass, including deliberately copying left input into the
private right buffer and copying left histories after a nonzero trajectory; both
fault controls break the relation while retaining stock left state/output/context.
Object build/conformance re-decodes 677 instructions; gen-check, regression
TypeScript and scoped lint/format checks pass. Private data is 4,020 bytes, code is
2,860 bytes, the largest observed identity fixture is 4,315 instructions and the
model-exercised store depth remains 172 bytes. These are bounded fixture metrics,
not performance or universal stack ceilings.

Both native modes pass from forced reloads of the same hashed DN2 1.12 snapshot.
Each records 2,400 render/shadow calls, one mirrored reset, 2,400 blocks with
nonidentity coefficients, 76,799 nonzero inputs and zero mode errors, nonfinite
observations, mismatches or fallbacks. Mode 1 additionally records 76,799 nonzero
opposite-sign audio relations and 4,800 opposite-sign live-history relations;
mode 0 records neither. Both latch numeric MODE1 `0x39011cf8`. Each original and
patched capture contains 4,800 stereo frames/100 ms, with zero alignment shift and
whole-WAV SHA-256
`6be721f8b1649c0a037bd8bd1a0998ff5847ed80de31b1907f98b01604555b0c`.
The audible stereo output therefore remains the original stock baseline in these
scenarios; the inverted right data is diagnostic private data.

The ignored native evidence is:

- Mode 0: `out/runs/dsp-stereo-proof/native/4c8488cb-3275-4c6e-a6f6-0a9dccef49d2/result.json`.
- Mode 1: `out/runs/dsp-stereo-proof/native/cefabd29-f00f-413c-a368-f8db89f1b179/result.json`.
- Invocation, source/binary hashes and log: `out/runs/dsp-stereo-proof/commands.json`
  and `out/runs/dsp-stereo-proof/native-polarity-acceptance.log`.

The prior allocation/headroom qualification and legacy core-label distinction
still apply. The U/Y-stage caller/context audit and separately approved bounded
implementation are recorded below; its combined native acceptance now passes.
Full-route capture/commit, cleanup, writer exclusion,
production backing ownership, public SDK ABI, hardware and performance claims
remain open. No new production policy was selected.


## U/Y integration caller/context audit and positive profile

**[D]** DN2 1.12 engine calls `0x1c959f` (early) and `0x1c9b7a` (late)
both enter `0xb81368` with the same record `0x2533e0+0x20*v`. Coefficients
occupy words 2..5 and live histories words 6/7. R4 supplies the record, R8=R12
the slot audio buffer, and `[I6+1]=R9` the count (normally 32). Reset call
`0x1c96aa → 0xb8135a` uses delayed setup R4=R5+R10 and clears words 6/7.
The early placement selects G=0 and the late placement G=1 from table
`DM8(0x254ac4+v)` with R11=0. A stable gate would make the placements exclusive,
but gate stability is not proved; both are reachable in the CFG. They therefore
require one persistent private right history per slot, captured controls/input
before each occurrence, and no reinitialization between placements. Both render
hooks and the reset must be covered; the helper's other five callers stay stock.

**[C]** The earlier four-byte gate stride was wrong. Original instruction fields
at producer `0x1c911a` and readers `0x1c9531/0x1c99a4` select byte accesses.
The producer's subtract-`0xf0`/postmodify-`0xf1` sequence advances one byte per
slot. The abbreviated listings hid this width; the gate is not a word array.

The immediate continuations do not consume returned I12/R2. The scalar predecessor
is `0x1c94b0`; the existing SIMD/context fallbacks must remain. These are bounded
caller facts, not a new public call ABI or all-context exclusion proof.

**[D]** The first fast profile contained no call rows, so it supplied no absence
evidence. A separately bounded interpreter profile disables AOT, fast regions and
block translation. In both stock and patched observations it records 2,400 early
callsite visits, one reset and 2,850 total helper-entry visits across all callers.
The late site has no row in this scenario; that does not establish it is unused
or that gate stability holds. The mode-0 native case passes, with mode 1
intentionally filtered. Both status records report zero AOT/fast execution and
zero traps; stock/patched stereo WAVs retain the same hash and zero shift.

Ignored evidence is `out/runs/dsp-stereo-proof/uy-interpreter-profile.e45298-1`
and `out/runs/dsp-stereo-proof/uy-interpreter-profile.e45298-2`, with
`out/runs/dsp-stereo-proof/uy-interpreter-profile.log` and
`out/runs/dsp-stereo-proof/native/fd1c110f-a444-4e5f-ad50-42d2b4a4d11d/result.json`.
Exact profile environment, filtered invocation and counts are retained separately
as `uy_coverage` in `out/runs/dsp-stereo-proof/commands.json`. The profile proves
positive coverage of early/reset in one finite scenario; it does not exercise the
late placement, install U/Y wrappers, establish full-route lifecycle or select
production backing/interrupt policy. The coordinator subsequently approved the
bounded implementation described below.


### U/Y private observer owner-check checkpoint

**[D]** The same opt-in proof module now hooks both audited U/Y render sites to
one shared observer and the local reset to its mirror. A separate appended source
retains the first-stage source unchanged; the combined model compares its linked
instruction prefix against the original alone at fixed placement/bindings. Sixteen
private 8-word records and 32-float buffers retain history across all occurrences.
Each occurrence captures coefficient words 2..5 and input before stock left runs;
first use alone clones the record and optionally inverts histories 6/7. Right
execution uses the original helper/operation order. Reset mirrors only 6/7 after
initialization. There is no per-site trampoline and no late-site execution claim.
The other five render-helper callers, helper bodies and all selected call delays
remain unchanged, including reset's computed argument delay instruction.

The shared private mode must be set before first accepted use. Each stage records
its own active/numeric mode; global mode errors refuse both until reload. U/Y
rejects nonfinite trajectories in both modes, while the first-stage source retains
its earlier identity exceptional comparison behavior. Finite identity/polarity
checks include audio and live histories, with signed-zero normalization. Stock
left state/output/status/frame remain authoritative; no audible output replacement,
IRQ-mask policy or public ABI is added.

The two named owner model files pass 75 cases: 39 first-stage and 36 U/Y. U/Y
covers both modes at slots 0/7/15, repeated modeled early/late occurrences on the
same record, changing controls, actual reset/retrigger and deferred initialization,
input/history corruption controls, nonfinite refusal, cross-stage mode switching,
count/slot/DAG fallbacks and SIMD handoff. Modeled occurrences are isolated helper
calls, not a claim that stock CFG gates naturally exercise both placements.
Combined private data is 6,760 bytes and code 5,830 bytes; all 1,375 instructions
re-decode. Build/conformance, gen-check, regression TypeScript, scoped lint/format,
layercheck and pathcheck pass. The observed U/Y fixture maximum is 3,038
instructions and deepest exercised model store is 176 bytes below entry frame;
first-stage metrics remain 4,315/172. None is a universal stack or timing bound.

The native fixture gates U/Y calls, mirrored reset, finite/error-free relation and
nonzero opposite-sign audio/history in mode 1 alongside the existing first-stage
gates. It computes scenario-specific headroom using the lower observed entry of
either stage minus the maximum exercised 176-byte model depth. Combined native
execution is coordinator-owned and passes as recorded below. Exact commands/current hashes are
retained under `uy_implementation` in ignored
`out/runs/dsp-stereo-proof/commands.json`; the old pre-hook coverage profile and
first-stage native passes remain separately qualified. Full-route lifecycle,
production backing ownership, hardware and exceptional/40-bit correctness remain
open; this is finite metamorphic independence evidence, not an independent oracle.


**[D] Combined native checkpoint:** both identity/polarity modes pass with the
first-order and U/Y observers installed. In each mode, each stage records 2,400
render/shadow calls and one mirrored reset, with zero mode errors, nonfinite
observations, mismatches or fallbacks. In mode 1, each stage records 76,799 nonzero
opposite audio and 4,800 opposite live-history relations. Stock/patched WAVs in
both modes have the same SHA-256
`6be721f8b1649c0a037bd8bd1a0998ff5847ed80de31b1907f98b01604555b0c`,
4,800 stereo frames/100 ms and zero alignment shift. The minimum observed entry
minus the model-exercised 176-byte depth leaves 3,128 bytes in the AudioTask
allocation. This is scenario-specific arithmetic, not a native store watermark or
worst-case IRQ/scheduler headroom. Shared U/Y counters do not attribute placement;
this Pre-only run did not observe the late site; the later panel-switch profile
below supplies positive coverage.

Ignored results are
`out/runs/dsp-stereo-proof/native/5e1a8014-6b17-47b1-b3c6-c1d26d167f99/result.json`
(mode 0) and
`out/runs/dsp-stereo-proof/native/a49fdec1-2e03-42a1-a0d0-58989c6c9ede/result.json`
(mode 1), with `out/runs/dsp-stereo-proof/native-uy-acceptance.log` and exact
unchanged environment/invocation in `out/runs/dsp-stereo-proof/commands.json`.
The finite same-MODE1, metamorphic, 40-bit/hardware, full-route and backing-ownership
qualifications above remain; no production policy is selected by this result.


## Legitimate BW Routing switch and native late-site coverage

**[C][D]** G is the byte table `DM8(0x254ac4+v)`, not a four-byte-stride table. The DN2
1.12 UI parameter's audited formatter uses raw>>8: zero displays Pre and nonzero Post,
with raw `0x100` selecting G=1. The current user manual
`Digitone-2-User-Manual_ENG_OS1.12_260930.pdf` (SHA-256
`53b4eec03935e4ab318449e474346712ec1ceffa2af0db91ec6b0404d024f9f3`), printed page 59,
places BW.RT at ENC G on FLTR page 2/2; printed page 60 describes Pre/Post. This
supplies a legitimate panel path rather than a routing-memory setter.

Two bounded stock-only probes calibrate the actual UI. With TRIG 1 held, FLTR taps at
100/150 ms (20 ms holds) followed by ENC G +1 at 200 ms, +8 at 300 ms and +32 at 400 ms
leave the route Pre after the first two turns and Post after the third. No
detent-to-raw-unit scale or acceleration is assumed. Early FLTR screenshots were stale
until redraw; the confirmed run checks the visible BW.RT caption/value at 390 ms (PRE
FLTR) and 500 ms (POST FLTR), matching byte gates 0/1 for all 16 slots. The initial
automatic NO event at 0..2,000,000 CF instructions is retained in recorded inputs. Only
two exploratory probes ran; each closed its own daemon. Evidence is under ignored
`out/runs/dsp-stereo-proof/route-ui-probe/` in runs
`6e78d99e-5bb6-4a42-80ef-75e1b56bf248` and `2246456e-0f72-400c-afef-aef08acbcf06`, with
full frame readbacks/PNGs, gate/history snapshots and exact recorded events.

The existing acceptance is parameterized by Pre/route-switch and identity/polarity,
preserving the original Pre timing. The route variant uses the same forced hashed
snapshot and exact instruction-scheduled panel changes for stock and patched captures;
TRIG 1 remains held from 1 ms through 800 ms. Early and 390 ms snapshots contain finite
nonzero histories; the initialized private slot, inverse/identity history relation and
reset count 1 persist through the switch. Calls continue after Post without
reinitializing the record. Pre/post diagnostic snapshots are read-only; the only routing
changes are ordinary panel inputs. The existing private proof mode remains configured
before first use, as in the earlier experiment.

All four native cases pass. Both stages retain zero mode errors, nonfinite observations,
mismatches and fallbacks, with one mirrored reset. The Pre cases retain 2,400 calls and
the prior 100 ms/4,800-frame stock WAV hash. Each route case records 19,200
render/shadow calls per stage and 38,400 stereo frames/800 ms. All four route WAV files
have SHA-256 `f88b7d008d22c6364c1925a986bbd61615a0859afe2bc1c150ddc04e6ccda61c`,
independently checked, with zero alignment shift. Mode 1 observes 614,399 opposite audio
and 38,400 opposite history relations per stage. Minimum native entry minus the maximum
176-byte model-exercised depth leaves 3,128 bytes in this scenario; this remains
arithmetic rather than a native store watermark or IRQ nesting bound.

Aggregate hook counters and gate bytes alone do not attribute a callsite. A separate
filtered interpreter route run positively records both placements:

| Site | Stock count | Patched count | MODE1 |
| --- | ---: | ---: | --- |
| Early `0x1c959f` | 10,160 | 10,160 | `0x39011cf8` |
| Late `0x1c9b7a` | 9,040 | 9,040 | `0x39011cf8` |
| Reset `0x1c96aa` | 1 | 1 | `0x39011cf8` |

Both status records have zero `aot_block`, `fast_tier`, `fast_runs`, `block_entries` and
traps. The interpreter case passes, with three other cases intentionally filtered,
retaining the same route WAV hash and zero shift. This closes the prior late-placement
evidence gap for this held-note scenario; it does not prove all routes, stable global
gates, asynchronous exclusion or full lifecycle behavior.

Ignored four-case results (Pre 0/Pre 1/route 0/route 1 respectively) are:

- `out/runs/dsp-stereo-proof/native/7b5c4f4c-989d-4e36-8f04-d2464df6f5bd/result.json`.
- `out/runs/dsp-stereo-proof/native/7a78de33-c497-4ea1-b6d9-91ffa6e63258/result.json`.
- `out/runs/dsp-stereo-proof/native/8fd8f1f0-8786-41e5-b118-1f580edffba2/result.json`.
- `out/runs/dsp-stereo-proof/native/d3bf40a3-f34b-4e72-9b0f-16b98c98bc64/result.json`.

The interpreter result is
`out/runs/dsp-stereo-proof/native/1ebcc4a6-1eb1-43e7-886f-a54a89a0b8b0/result.json`;
profiles are `out/runs/dsp-stereo-proof/route-interpreter-profile.e39898-1` and
`out/runs/dsp-stereo-proof/route-interpreter-profile.e39898-2`. Logs are
`out/runs/dsp-stereo-proof/native-route-acceptance.log` and
`out/runs/dsp-stereo-proof/route-interpreter-profile.log`. Exact environments, commands,
probe calibration and TS hash are in `route_extension` under
`out/runs/dsp-stereo-proof/commands.json`. No DSP, SDK or ABI change is made; finite
same-mode/metamorphic, hardware, 40-bit, performance and production-backing
qualifications remain as above.
