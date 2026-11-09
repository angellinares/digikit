# DN2 DDR geometry and reservation boundary

2026-10-29, DN2 OS 1.12. **[D]** inspected evidence; **[O]** unresolved ownership.
An independent evidence agent checked original boot-stage bytes and public
[SHARC references](../sharc/SOURCES.md), for the [sampler plan](../plans/dsp-sdk-and-sampler.md).

**[D]** Boot INIT entry `0x120230` calls `0x120510`, then `0x120353`.
At `0x120520`, the initialization table is `0x242c4c`; the store at `0x120362`
writes its first word, `0x0af00622`, to DMC0_CFG (`0x31070040`). The table was
read from the original initialization-stage block before final loading overwrites
that memory. Using final loaded memory here would lose the configuration evidence.

**[D]** ADSP-2156x Hardware Reference Table 9-6 (printed pp. 9-17–9-18) decodes
low fields `0x622` as one external bank, 4-Gbit SDRAM, 16-bit device/interface:
512 MiB configured geometry. This is firmware configuration, not a hardware
capacity measurement or an allocation of that capacity to samples.

**[D]** Boot blocks 84–93 end their DDR loaded/fill extent at `0x8052fbe0`.
Stock code at `0x1c4664` uses that same address as a scratch base; the SIMD
clear loop at `0x1c4673..76` and subsequent normalization helper `0x1c45de`
read/write beyond the loaded extent. Therefore even the end of the loaded image
does not establish the start of free DDR.

**[D]** Audited allocator initialization `0xb89c53` constructs a free-list arena
`[0x2d08b8,0x2dd0b0)`, 51,192 bytes in L1 private byte-address space, with metadata
at `0x2dd0b8..d8`. Its `0x80000000` constant is an allocation flag, not a DDR
base. This allocator cannot be used as evidence for a larger sample pool.

**[C]** The earlier normal-address-space description was incorrect: allocator
`0xb89c9c` takes a byte count and returns a private L1 byte pointer, not a
normal-word address. The arena extent is not available capacity. Its audited
failure path is fatal rather than a recoverable NULL result; see the
[L1 staging audit](95-dn2-cache-and-lp0-staging.md#l1-staging-option-and-allocator-failure).

**[O]** The five-root pass establishes no general DDR allocator, maximum dynamic
scratch extent or production reservation hook. The existing emulator bank and a
larger high-DDR candidate still need a lifetime/ownership account. The next
question is runtime pointer provenance and bounds, rather than aperture size.
No DT2 project-pool or waveform-workspace addresses were transplanted to DN2.

**[D]** Original DN2 1.12 section-7 SHA-256:
`336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`.
No emulator run, hardware contact, binary changes or firmware bytes are part of
this finding.


## Bounded runtime-pointer follow-up

**[D]** Original-byte inspection bounds the audited scratch loops to
`[0x8052fbe0,0x80530be4)`, 4,100 bytes: 512 SIMD pair stores at eight-byte stride,
then one word. Reusers at `0x1c4745`, `0x1c4875` and `0x1c4999` share the window;
normalizer `0x1c45de` operates on its first 4,096 bytes. The recorded startup
call path is `0x1ca76b → 0x1c1445 → 0x1c8e1c → 0x1c8e67 → 0x1c463b`.
Missing direct audio-task edges do not exclude indirect use.

**[D]** In-memory graph analysis of current database edges and store sites found
3,229 stores in the audio-task direct graph, 729 resolved addresses, and 1,136
unresolved stores after excluding obvious stack accesses. Those unresolved sites
are provenance gaps, not evidence of high-DDR writes. The pointer table can also
retain stale values through floating-point index arithmetic: an apparent high
address at `0xb811a5` resolves on instruction inspection to a table at `0x8052dbe0`
with index clamped to 0..1023. Neither pointer-table absence nor a raw address-shaped
literal is sufficient to reserve a pool.

**[O]** Candidate high DDR `[0x9e000000,0x9f000000)` is within configured geometry,
but no reservation is established. A bounded experiment would need an explicit
owner/extent, checked destinations and stock CPU/DMA access guards; production
ownership still requires the remaining provenance/lifetime account.

## Fixed FX object follow-up

**[D]** A further original-byte audit ties allocation entry `0xb89c9c` to the
known L1 heap: its initialization pointer is `0x2dd0c4`, lazy initialization at
`0xb89de1` calls `0xb89c53`, and task creation calls it at `0xb88853/69`.
This does not establish a reservable DDR heap or prove every allocator was found.

**[D]** Boot passes `0x80000018` to `0xb83dc1`; its valid-rate continuation
`0xb83dfa` clears `0x59114` bytes through `0x1c0bde`, ending exactly at the next
object root `0x8005912c`. Initializer `0xb848cb → 0xb8450e/b84604` establishes
this next object's layout:

| Region | Half-open byte range |
| --- | --- |
| Ring 0 | `0x80059130..0x80259130` |
| Ring 1 | `0x80259130..0x80459130` |
| Work 0 | `0x80459130..0x804591b0` |
| Work 1 | `0x804591b0..0x80459230` |
| Control tail | `0x80459230..0x80459388` |

The next boot object begins at `0x80459388`. The initializer clears both rings
using `0x40000` iterations of two SIMD pair stores (`0xb84571/74`); before that,
32 SIMD pair stores (`0xb84521..5f`) clear the 256-byte workspace. Reset `0xb8439b` has its highest
explicit control store at `0xb8440a`, ending at that next root. Ring index masks
at `0xb846ce/d6`, `0xb8431b` and `0xb84071/77` are 19-bit masks, bounding
valid-state accesses within the two 2 MiB rings. Render entry is reached through
`0x1c1c4a → 0xb8482c`.

**[O]** Workspace loops consume live frame count at `0x257e6c`; boot initializes
it to 32 at `0x1c14a8`. A universal runtime count bound and the ownership of
caller-provided output pointers remain separate obligations. These object-local
bounds do not establish a free production pool.

**[C]** A pointer-table row suggesting `0xb823b3` writes the frame count retains
stale R12 provenance. Although caller `0x1c3038` passes `0x257e6c` into
`0x1c8ef1`, the relevant dispatch reloads R12 at `0x1c958c` from a runtime
output-pointer table rooted at `0x254a60`. Switch-table entry 1 reaches
`0x1c98ab`, which calls `0xb82342`; its output store `0xb823b3` uses that
reloaded pointer. This is not positive evidence of a frame-count write. The
pointer table is loader-zero and its populated values still need tracing; no
universal no-alias claim follows.

**[D]** The next root `0x80459388` is bound by boot `0x1c168f/95` to
initializer `0xb84f51` and by frame code `0x1c1bd5/e1` to renderer `0xb84d5c`.
It has a four-byte header, two 2,048-byte rings
`[0x8045938c,0x80459b8c)` and `[0x80459b8c,0x8045a38c)`, two 128-byte work
buffers ending at `0x8045a48c`, and checked control accesses ending at
`0x8045a6c4`. The next root is `0x8045a6c8`: the four-byte remainder is not
claimed free. Ring clear count 256 and SIMD stores at `0xb84f39/3c` match the
ring extents; workspace stores at `0xb84e1a..58` clear 256 bytes. Nine-bit masks
at `0xb84ba1/ac/b2/b8` and `0xb84bef/f2` bound initialized, serialized read/write
indices to 0..511. Highest checked reset store `0xb84edf` ends at object offset
`0x133c`. Work-buffer bounds again depend on live frame count ≤32; caller
output-pointer arrays remain a separate obligation.

**[D]** The following data root `0x8045a6c8` is read at `0x1c297d` as
`base + 512 * group + 4 * index`. The signed selector loaded at `0x1c293a`
is clamped to group 0..4 at `0x1c2947/4c`. **[C]** The separate per-track load
at `0x1c2940` was initially described as an unbounded packed word. Raw Type3b
fields `l=1,w=0,x=1` instead establish a sign-extended 16-bit load, advancing
I5 by two bytes. Arithmetic right shift by eight at `0x1c2942` therefore bounds
the index to -128..127 regardless of CF validation. All resulting four-byte
reads lie within `[0x8045a4c8,0x8045b0c8)`. The source is frame halfword offset
`0x34 + 2 * track`, not a word in the `0x92`-stride parameter block.
The raw loop trip count at `0x1c2937` is 16. CF writer `0x40027510` copies
the halfword from `0x8000dd40 + 2 * track` into frame location
`0x80005e94 + 2 * track`; the upstream value producer is not needed for the
signed-load bound and remains unaudited.
**[O]** A nonnegative 0..127 range still needs the producer contract, but is not
needed to exclude arbitrary high-DDR addresses from this particular lookup.
Loader block 89 covers `[0x8045a6c8,0x80467cd0)`, which proves loaded extent,
not a runtime access maximum or free space.

**[C]** Pointer propagation also needs packed-access width handling. Original
instruction `0xb84597` has raw fields `l/w/x=0/0/0`, a byte access. Its `0xd3`
post-modifier advances from object offset `0x400119` to `0x4001ec`. Applying
blanket four-byte scaling would invent an overrun; raw access semantics control
the bound, not the pointer table's estimate.

## Output-pointer table initializer

**[D]** A targeted follow-up resolves the initializer of the previously open
table at `0x254a60`. The parent traced the computed field through current query
listings; an independent agent checked the original DN2 1.12 section-7 bytes
against the SHA above and the public Programming Reference.

Boot `0x1c14a2` passes base `0x241298` to `0x1c8e1c`. Its valid-rate branch at
`0x1c8e42` is delayed: both setup instructions at `0x1c8e45/48` execute before
continuation `0x1c8e67`, setting R11=0 and preserving the base in R5. The loop
calls `0x1c8b6d` at `0x1c8ecc` with that base and track index 0..15. Callee
register preservation and the increment/comparison maintain those values.

The callee forms the table address as `base + 0x1387c - 0xb4`. At `0x1c8bfd`,
the parallel memory operation stores incoming R2 while the compute operation
produces a new shifted R2 for later use. Normal-word premodify addressing scales
the track modifier by four without updating the table base. The resulting mapping is:

```text
table[track] at 0x254a60 + 4*track = 0x804acf90 + 128*track
track = 0..15
```

The table occupies `[0x254a60,0x254aa0)`. Sixteen SIMD pair stores at
`0x1c8c18..36`, with eight-byte advancement, clear 128 bytes per target.
The combined initialized output region is `[0x804acf90,0x804ad790)`, 2,048 bytes.
Public semantics: [SHARC+ Programming Reference](../sharc/SOURCES.md), Rev 1.5,
printed pp. 3-2, 3-22 and 14-12–15. The separate pointer assigned at
`0x1c8c05/0b/0d` is `0x804ad790 + (track << 15)`; this establishes pointer values,
not a 32-KiB usable allocation or every consumer's access bound.

**[C]** Two analysis traps matter here. A function-boundary label at `0x1c8e67`
does not make it independent of the earlier branch's delay-slot setup. Also,
the table has sixteen entries, not eight: `0x254a84` is slot 9, inside it.

**[O]** This closes initializer provenance, not the table's whole lifetime.
The database's resolved store candidate `0x1c9118 → 0x254a84` needs instruction
and caller-state tracing before treating it as either a real table mutation or
stale pointer propagation; packed access width and intervening I2 reloads matter.
Other computed writers and consumer bounds remain open. No production DDR
reservation or free-space conclusion follows from this initialization mapping.

**[C]** Original-byte follow-up resolves that specific `0x1c9118` estimate on a
bounded first-pass path. The instruction is a Type4b **byte** store with signed
offset -16 and premodify addressing; it leaves I2 unchanged. The following
`0x1c911a` is also a byte store, at current I2, then advances by `0xf1` bytes.
Applying a four-byte scale to either modifier invents the table overlap.

The dispatch prologue saves `base + 0x1380c` to stack slot -31, reloads it into I2
at `0x1c8fef`, adds `0x110` at `0x1c9032`, then subtracts `0xf0` at `0x1c90f6`.
On the selected route through `0x1c90b2`, normal return from `0x1c4f04`, direct
SV branch `0x1c90c8 → 0x1c90d4` and EQ fallthrough at `0x1c9104`, the store is at
`base + 0x1381c = 0x254ab4`, outside the table ending at `0x254aa0`.
The intervening callee saves I2 at `0x1c4f17/19` and restores it at `0x1c511f`;
its checked continuations share that epilogue and do not overwrite the saved slot.
There is no earlier call on that chosen dispatch route. An independent agent
checked these instructions in the original image and public byte-access semantics
(Programming Reference printed pp. 14-29–32 and 6-9–12).

**[O]** Other branches through `0x1c4e5c` or `0xb82440`, later loop recurrence,
indirect paths and other writers remain outside this bounded check. The local
subtract/postincrement suggests one-byte progression when every pass takes this
path and preserves I2; it is not a universal lifetime proof. This correction
removes a false positive, without declaring the table immutable or DDR free.

## Per-track initialization extent

**[D]** Further independent original-byte inspection of the same DN2 1.12
section-7 image follows `0x1c8d70/73` (per-track record) → `0xb80575` →
`0xb80546`, called at `0xb80593`. At `0xb80551`, I5 loads record word 16
(offset `0x40`). The helper sets I12 to zero at `0xb8054f`, enables SIMD at
`0xb80553`, sets M4=2 at `0xb80556`, and uses literal DO count 4096 at
`0xb80558`. The SIMD pair of 32-bit stores at `0xb8055b` advances eight bytes.
This clears 32,768 bytes at each pointer `0x804ad790 + 0x8000 * track`,
track 0..15: combined initialization extent `[0x804ad790,0x8052d790)`, 512 KiB.
Public semantics: [Programming Reference](../sharc/SOURCES.md), Rev 1.5,
printed pp. 14-12–15 and 15-23.

**[C]** The pointer values previously established only provenance; this call
path adds a positive initialization extent. It does not prove all runtime
bounds, a usable sample allocation or free space. No explicit 8,192-word length
or ring mask has been established in the descriptor. **[O]** Adjacent helpers
`0xb80e48`/`0xb80e82` and runtime consumers remain to be audited.


## Runtime ring consumer: conditional bounds

**[D]** Independent original-byte inspection of DN2 1.12 section 7
(SHA-256 `336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`)
follows consumer `0xb806f5`, called from `0x1c9b73`. It loads record word 16
into I1 at `0xb80781`. Reads at `0xb8079b/9e/a4/a7` and the write at
`0xb807dc` use 32-bit normal-word premodify accesses: four-byte DDR index
scale, without advancing I1. Later read/write indices are masked to 13 bits
at `0xb807e8/ee/f4/f7/fa`, and the write index is saved at `0xb80805`.

The initial reads need a separate bound. R3 is TRUNC(record word 13), R5 is
TRUNC(float(record word 14) minus record word 15, adding 8192 if negative),
and the initial write index is record word 14. Only their adjacent read
indices are initially masked. The loader's zero FILL extent
`[0x28241290,0x2825d2b8)` covers word 13 for all sixteen records
(byte alias `0x28253614 + 0x48 * track`); the checked constructors/reset omit
that word. **[C]** The same-family stride is `0x48` bytes (72 decimal), not
48 decimal bytes; the bounded R4 provenance audit below resolves the earlier
ambiguous notation. Adjacent `0xb80e48` clears record word 0, `0xb80e82` initializes
coefficients, and `0xb805d8` resets record word 14: these are not allocators.

**[O]** The delay clamp at `0xb80607` to 1..8191 alone does not prove the
first read stays inside 32 KiB. With write index 1 and delay `1 + 2^-23`,
round-to-nearest can turn the wrapped value into exactly 8192 before TRUNC.
This is a counterexample to that inference, not an observed device overrun.
Producer quantization, rounding mode, finite input guarantees and SIMD state
across prior calls remain to be established. The dispatcher clears PEYEN at
`0x1c94b0`, but that alone does not establish its value at every consumer call.

Under SISD, the correct pointer, serialized initialized state and initial
indices in 0..8191, accesses fit `[base,base+0x8000)` and the masked recurrence
preserves the bound. Allowing the finite rounding endpoint requires at least
`base+0x8004` for the first iteration; SIMD could add an adjacent lane.
Public semantics: [Programming Reference](../sharc/SOURCES.md), Rev 1.5,
printed pp. 6-9–12, 14-12–14, 20-13, 20-20 and 24-11. This narrows a stock
buffer's use; it establishes neither free DDR nor a production sample pool.

## Existing observation limits

**[D]** The emulator's `SHARC_WWATCH` in
`packages/emulator/sharc/src/write_watch.rs` samples at most 64 selected words
after each dispatch unit. It detects changed values, not every access: reads,
same-value writes and addresses outside the selection are invisible. Generated
block/fast-tier observations identify the dispatch entry, not the exact writer.
The [instrumentation guide](../INSTRUMENTATION.md) documents interpreter-only
settings for finer attribution. Thus a quiet watch is not a range-exclusion proof.
The bounded source scan found no existing complete CPU/DMA range guard; coverage
would need to account for block fast paths and peripheral memory access as well
as general read/write helpers. No instrumentation or core regeneration was done.

## Named DMA boundary follow-up

**[D]** Current DN2 1.12 database/source inspection narrows two candidate routes
to local buffers, rather than finding a DDR allocator. In
`0x1c9e76 → 0x1c9d6b → 0x1ca020`, the helper receives addresses of stack locals
and fills them with ping-pong selections rooted at `0x2c29d0` and `0x2c49d0`.
The toggle is `DM(0x2c0450)`; these are the stock L1 receive/reply roots already
identified in [finding 88](88-dn2-reply-ownership.md). Their main reply descriptor
uses `0xabc` bytes / `0x55e` halfwords, not an arbitrary DDR destination.

Route `0x1ca59f` initializes auxiliary pointers `0x2c34c0`, `0x2c44c0`,
`0x2c54c0`, `0x2c64c0` at `0x1ca5b3..da`, matching the main roots plus `0xaf0`
in finding 88. The observed `0x100000` control writes denote DMA interrupt
configuration, not a transfer extent. The auxiliary setup audit below bounds
transfer direction/count; actual invocation and runtime lifetime remain unresolved.
These fixed roots narrow the owner table, not free DDR or sample capacity.

## Actual AudioTask entry versus database function ownership

**[D]** A bounded DN2 1.12 original-byte audit checked 18 instruction heads in
section 7 (SHA-256
`336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`).
Task creation loads the name pointer at `0x1ca00b`, R4=`0x1c9fe7` at
`0x1ca00e`, then calls `0xb88839` at `0x1ca013`. The entry is forwarded through
`0xb88885 → 0xb87b58 → 0xb8af28` to initial-context store `0xb8af71`.
Thus `0x1c9fe7` is the actual task entry; the earlier database-owner route
starting at `0x1c9e76` is not an entry-address claim.

The actual loop waits at `0x1c9fe7`, calls dispatch `0x1c9d6b` at `0x1c9ff4`,
and reaches command-table target `0x1c9f0f`. The chain continues through call
`0x1c9fbc → 0x1c2712`, call `0x1c3044 → 0x1c8ef1`, then call
`0x1c9459 → 0xb8251b`. The table at byte address `0x28268a68` contains four
targets: `0x1c9dc2`, `0x1c9e76`, `0x1c9eda`, `0x1c9f0f`; its indirect
transfer is `0x1c9dbf`.

**[D/O]** The database merges ownership into `[0x1c9e76,0x1ca000)`, so the
whole-containing-function store report overapproximates around the actual entry.
Unfollowed edges also prevent complete reachability. All 23 CALL rows lacking a
database target function have concrete target PCs: 22 have a unique containing
range, one has none, and none is ambiguous. Examples are
`0x1c9fe7 → 0xb891fa` (interior of `0xb891eb`), `0x1c95b4 → 0xb8265b`
(interior of `0xb8258f`), and `0xb80e6a → 0x1c125e` (no recorded owner).
The bounded helper audit below addresses `0x1c125e`; explicit entry/dispatch
precision remains future work before any query-policy change. No tool policy is
expanded here; this establishes no runtime
store bound, full closure, free DDR or backing ownership.

## Callable helper missing database ownership: `0x1c125e`

**[D]** A further bounded audit checked 64 original DN2 1.12 instruction heads,
including widths, forms and raw encodings, against the same section-7 hash above.
`0x1c125e` is callable despite lacking a database owner: direct caller
`0xb80e6a` reaches it; other recorded callers `0xb810e8` and `0xb81f2e` were
not expanded. Followed body ranges are `0x1c125e..0x1c126c` and
`0x1c127f..0x1c12df`, skipping adjacent entry `0x1c126d`. Saved-return I12 use
at `0x1c1260` precedes jump `0x1c1266` with RFRAME in its delay sequence.
The conditional return at `0x1c1280` is non-delayed despite its database delay
label; final return `0x1c12d7` has two delay instructions.

The followed helper body has no stores or further calls. Its memory operands
read saved-frame state and fixed normal-word table `0x90123` (byte alias
`0x2824048c`); the remaining work is floating-point math. This does not describe
the skipped adjacent entry or expand the other callers.

In immediate caller `0xb80e54`, registers are saved through I6-relative stores
at `0xb80e5b/60`, with I7-relative frame stores at `0xb80e6d/6e`. The nonstack store is `DM(I5+2)` at `0xb80e79`.
I5 copies incoming R4 at `0xb80e56`, and the store follows another call at
`0xb80e71`. The following provenance audit resolves this bounded caller's R4
and word-2 destination. No database repair or traversal-policy change is made;
the helper's store-free body alone does not establish complete reachability or
backing ownership.

## Caller destination closed: per-slot voice state, not ring backing

**[D]** The next bounded DN2 1.12 audit checked 214 original instruction heads
(widths, forms and raw encodings), using the same section-7 hash above. Engine
setup `0x1c303e` supplies R4=`0x241298`, retained in I9 at `0x1c8f18/1d`.
Frame slot I6−4 holds that base; I6−13 holds the slot byte offset, initialized
to zero and incremented by `0x48` (72 decimal) at `0x1c9351/57`. The slot counter
starts at zero, increments at `0x1c927e`, and the four-bit termination at
`0x1c934e` bounds valid initialized slots to 0..15.

Dispatch table `0x8052dba4` identifies `0x1c977b` and `0x1c984e`. Both supply
R4=`base + 0x12348 + slotOffset` to `0xb80607` at `0x1c97b7/0x1c988b`;
the former sets R4 in delay instruction `0x1c97ba`. `0xb80607` preserves R4
through `0xb80660 → 0xb80e54`; `0xb80e56` copies it to I5. Store `0xb80e79`
therefore targets record word 2. The audited normal-word `+2` is an eight-byte
offset: record addresses are `0x2535e0 + 0x48*v`, with four-byte word-2 targets
`0x2535e8 + 0x48*v`, last target `0x253a20` for v=15. Canonical byte aliases
add `0x28000000` to these local addresses.

The alternate `0xb80e82 → 0xb80e54` path forwards the object; parent constructor
`0xb80575` uses the same layout. Setup `0x1c8bcb/da` independently corroborates
`base + 0x12348 + 0x48*slot`. This closes the bounded destination-family gap:
`0xb80e79` writes known per-slot voice state, not external ring backing. The
external ring pointer is separate record word 16 in
[finding 96](96-stereo-machine-processing.md); this word-2 store does not
dereference it. **[O]** External ring ownership, corrupted-state behavior, complete
writer reach and exact-entry query precision remain unproved. No database repair,
production policy or reservation is selected.

## Auxiliary SPI1 DMA configuration and remaining lifetime boundary

**[D]** A bounded audit checked 334 original DN2 1.12 instruction heads plus
original tables against the section-7 image above. Driver table `0x268d08`
selects SPI1 base `0x3102f000`, DMA24 TX `0x3102d100` and DMA25 RX
`0x3102d180`. Creator `0x1ca63e` uses device index 1. Table `0x2c0464` binds
TX ring `0x2c0040 ↔ 0x2c005c` to source buffers `0x2c64c0/0x2c54c0`, and
RX ring `0x2c0008 ↔ 0x2c0024` to destination buffers `0x2c44c0/0x2c34c0`.
Setup `0x1ca5b3..0x1ca5e2` installs their starts and circular links.

Creator `0x1ca648` takes the unsigned low 16 bits of `NbyteSize`, then
`0x1ca698 → 0x1ca59f` configures C=N>>1 at `0x1ca5e5`. Sites
`0x1ca603/606/609/60c/60f/612` establish XCNT=C and XMOD=2. SPI setup
`0x1cb672` selects 16 bits; `0x1cb342..360` selects two-byte DMA memory and
peripheral widths. For valid nonzero C=1..32767, each fixed root has configured
contiguous span `[root,root+2*C)`, at most 65,534 bytes. That is not actual buffer
size or coexistence proof: the roots are only `0x1000` bytes apart.

**[D/O]** XCNT=0 is a configuration error, not an empty transfer or evidence of an
inactive channel. The [public Hardware Reference](../sharc/SOURCES.md)
(`adsp-2156x-hwr.pdf`, Rev 1.0) supplies DMA channel assignment in Table 27-2,
printed p. 27-4 (PDF page 1239), count/modify fields in Table 27-10, printed
p. 27-27 (page 1262), WNR direction at p. 27-57 (page 1292), and zero-count
configuration error at p. 27-23 (page 1258). No hardware behavior was measured here.

Submission follows `0x1ca62c → 0x1cc019`; circular hardware continuation is not
inherently boot-only. Initialization registers DMA24/25 handlers `0x1cc656` and
`0x1cc6b8` at `0x1cc847/862`. They acknowledge/reset through `0xb8e210`, clear
request fields and can invoke an indirect callback. The lifecycle follow-up below
finds a linked shutdown/control path, while actual creator invocation and N remain
unresolved. Running on every boot or forever is not established. This
narrows direction/count evidence, not free DDR, complete writer reach, backing
ownership or a production reservation.

### Bounded lifecycle audit: shutdown path found, creator still unresolved

**[D]** A further audit checked 130 original DN2 1.12 instruction heads. No
recorded CALL/JUMP into `0x1ca63e` or its interior, decoded instruction naming its
entry, literal/data reference, or exact little-endian 32-bit entry pointer in final
memory was found. These negative searches do not establish dead code: computed
or external entry and missing decode remain open. Actual invocation and N remain
unresolved.

Positive path `0x1ca81b → 0x1ca6d9` registers callback `0x1ca6a7` at
`0x1ca72d`, through `0xb8c33a` at `0x1ca734`. This follows separate main SPI2
initialization at `0x1ca814` with R4=`0xabc`; that argument must not be attributed
to auxiliary creator `0x1ca63e`. Callback `0x1ca6a7` reads the auxiliary handle
from `DM(0x2c0078)` and calls `0x1cbac0`.

That control path checks request-state fields `+7/+8`. Active paths clear TXCTL
at `0x1cbb3f` or RXCTL at `0x1cbb22`, then reach cleanup `0x1cba88`.
Successful continuation clears SPI_CTL enable bit 0 at `0x1cbb05/07`; complete
error-path cleanup is not established. The same published auxiliary handle is
therefore positively linked to shutdown/control, but its external trigger, valid
publication and successful execution are not proven. No linked descriptor
start/count replacement was identified in this bounded audit.

**[O]** The static lifecycle slice is complete within that scope. The next boundary
is external/computed creator entry or a capability-supported exact-entry R4
observation; diagnostic tooling is being checked, with no new run selected here.
The valid nonzero configured-span ceiling remains 65,534 bytes and XCNT=0 remains
a configuration error. Neither every-boot/perpetual activity nor backing ownership,
free DDR or a production reservation follows.

## Canary map of stock DDR writes (2026-11-01)

**[D]** Tool `tools/emulator/ddrcanary.py`: `fill` writes an address-derived
canary into every DDR word the ready state has not written; `map` lists the 4 KiB
pages where the canary changed. Both work on the DSP state of coupled runs
(`--dump-dsp`, which includes DDR). Ready input: `out/snapshots/dn2-audio-ready-cold-2026-10-20`,
DN2 OS 1.12. The ready state wrote these ranges: `[0x80000018,0x80459247)`,
`[0x80459248,0x8045a6c4)`, `[0x804ace90,0x8052d790)`, `[0x8052fbe0,0x80530be4)`.

**[D]** The gap `[0x8052d790,0x8052fbe0)` (9.3 KiB) was never written. It is
stock-owned: with a canary in it, the DSP halts within 3 s (indirect jump to the
undecoded address `0xf682b3`). Stock code reads this gap and expects zero. This
fits the `0x8052dbe0` table noted above. The fill excludes the gap.

**[D]** 20 stock scenarios wrote no page in `[0x80531000, 0xa0000000)`
(518,972 KiB, about 507 MiB):

- 14 from `tests/scenarios/dn2.json` and `dn2-fx.json` (2.4-10.5 s each): idle,
  trig1-hold, kb-chord16, trig1-taps, trig1-tap-release, dn2-rec-play,
  dn2-rec-noplay, fx-del-hold, fx-dmp-rev, fx-chr-127, fx-over-127, fx-srr-127,
  fx-br-127, fx-dmp-chr.
- 6 from `tests/scenarios/dn2-soak.json` (generator `tools/emulator/dn2_soak.py`):
  save project, load project, new project, menus, FX combination, and a 60 s
  four-track pattern with p-locks, sound locks, LFO and sends.

Stores were seen elsewhere: 90-633 changed stock words per run, and 90,882 for
reverb. All are inside the ready-written ranges.

**[O]** Limits. Reads are not seen. A store of exactly the canary value is not
seen. Not covered: arpeggiator, song mode, audio input sampling, manage projects,
format, OS upgrade, loading a large project, play longer than 62 s. The 1,136
unresolved stores are not ruled out. This measures reachable behaviour, not
ownership.

**[O]** Proposed DN2 reserved range for the sample pool: start with 256 MiB
inside `[0x80531000, 0xa0000000)` in the device profile, with a hardware
read-back check later.
