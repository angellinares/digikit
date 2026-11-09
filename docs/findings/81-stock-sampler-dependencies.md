# Stock sampler dependencies and the next SDK proof

2026-10-29. Targets: DT2 OS 1.17 and DN2 OS 1.12. **[D]** records current-database/source observations;
**[O]** records unproved semantics or proposed work. **[V]** is restricted to the independently audited
original-image byte facts below; it does not cover the broader dependency/lifetime interpretation.
This updates the next-step framing in [finding 80](80-sampler-capacity-and-replacement.md), preserving its
capacity measurements and the completed immutable-bank proof in [finding 79](79-sample-bank-proof.md).

## Direction

The user wants stock DT2 OneShot and other machines to reveal the dependencies the SDK needs, rather than
wholesale copying of a stock machine. **Current requirements, corrected 2026-10-30:** full stereo on DT2
and DN2, 16 tracks sharing one project sample pool without fixed per-track partitions, and deduplicated backing
for references to the same asset. Long samples are bounded by actual capacity and supported format limits,
not ten seconds. Stock DT2-like project capacity is the target, not proven parity. Selecting a sample stops
only the selected track's old notes. The earlier mono/ten-second scope is historical prototype scope only.

Keep one shared SDK/sampler engine and runtime-shipped device adapters. The payoff is project-wide stereo
sample access with shared accounting. Stereo representation, achievable capacity and production ownership
remain open. The proposed 16 MiB DT2 tail recommendation is withdrawn; there is no outstanding reservation
choice and no approved reservation patch.

**[O]** The coordinator proposes an independently written shared **packed-PCM16 bounded reader, cursor and
region semantics**. A device adapter supplies a ready immutable view; runtime owns slot selection/loading and
lifetime. Stock downstream envelope/filter processing remains in place. DT2's direct stock view cannot be handed
to the existing float32 normal-word reader in `sample.inc`: representation and pointer contracts differ.
This is a design proposal, not an implemented ABI or a claim of stock-quality equivalence.

## DT2 current-image dependency map

| Dependency | Current DT2 1.17 observation [D] | Open boundary [O] |
| --- | --- | --- |
| Project acquisition / allocation | General loader `0x4015481c` accepts slots ≤`0x3ff` except multiples of `0x80`; rounds allocation as `(size + 0x200f) & ~0x1fff`, with offset limit `0x19000000` (400 MiB), and deduplicates references. | Allocation ownership, safe adapter entry and all failure paths require controlled proof. |
| Slot → block / delivery | Slot map `0x405b5368 + slot*4` reaches block vector `0x405a0f64..0x405a0f68`, stride `0x34`. Pages are sent by `0x40153a10`; headers by `0x40153e6c` / `0x40153e04`. | Page/header completion and cache/read visibility are not a reader-lifetime barrier by themselves. |
| Release / project reset | `0x401543a2` decrements references, coalesces on zero and clears the header through `0x40153ec6`. Reset `0x40154634` clears mappings and allocator; `ProjectSettings::ReloadAllSamples` at `0x4004e8be` queues worker work then resets/reloads. | Active/queued voice behavior across these operations needs capture before an adapter borrows a view. |
| Preview correction | `0x40154112` is preview: fixed slot `0x400`, offsets `0x19000400` / `0x19100400`. | **[C]** This is not the general project allocator; do not build the SDK contract from that earlier lead. |
| Kit selection | `0x4004e598` uses record `+0x80` for sample slot and `+0xd6` for machine type. | Precise frame `0xe0 + 0x60*t` remains a historical derivation, not a newly established current-image field contract. |
| Slot → voice setup | `0x1c3289` → slot lookup `0x1c3f78`, continuation `0x1c3fa8` → `0x1c7442` → `0x1c4e70`. Slot table candidate `0x257810`, rebased address `0x8422b9c8`; page copy `0x1c403c`. | Capture a real selected slot and resulting voice pointer. At `0x1c4e91`, `DM(I4,M4)=R8` with `M4=0x65` is post-modify: the store is word 0, **not word +0x65**. |
| Render branch | Dispatcher `0x1c642a` routes non-Stretch through `0x1c4ecf` → `0x1c4f81`; selector 2 Stretch goes `0x1c5576` → `0x1c5615`. | Specific OneShot naming and parameter mapping remain historical DT2 1.16 evidence until current controlled selection. |
| Reader / boundaries | Six shortword taps at `0x1c50b4`, `0x1c50c2`, `0x1c50cd`, `0x1c50d2`, `0x1c50d7`, `0x1c50db`; current database has a 64-iteration loop and coefficients at `0x25d940`. Header path `0x1c7e76` → `0x1c400f`. | Exact guarded span is unresolved. Detailed Q31 boundary, reverse and stop semantics in finding 06 around line 3518 are historical; do not promote them to current-image proof. |

### Independently audited instruction bytes

**[V]** A separate auditor decoded freshly extracted original DT2 1.17 image bytes. Stock SYX SHA-256:
`26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5`; section 7 is 321,016 bytes,
SHA-256 `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`, matching the extraction.
The verified scope is:

- `0x1c3fc6` raw `16448422b9c8` and `0x1c3fcb` raw `164d8422b9c8` modify by adding `0x8422b9c8`.
- `0x1c4e8f` raw `0fa40065` loads M4=0x65. `0x1c4e91` raw `893e84001f20` is the unconditional Type 6a
  post-modify store through I4/M4 of R8: store at the original pointer, then modify it.
- The six taps are Type 3b signed-shortword loads (`l=1,x=1,w=0`, SWSE): `0x1c50b4=497e40bd`,
  `0x1c50c2=577e413d`, `0x1c50cd=497e42bd`, `0x1c50d2=497e403d`, `0x1c50d7=59be40bd`,
  `0x1c50db=597e413d`. Their `u` bits are mixed: 1 at `0x1c50c2`, `0x1c50d7`, `0x1c50db`, and 0 at
  the other three. Do not infer uniform addressing mode across the taps.

The public [SHARC+ PRM revision 1.5](https://www.analog.com/media/en/dsp-documentation/processor-manuals/sc58x-2158x-prm.pdf)
Type 3b BHSE tables (printed 14-19/20) and Type 6a tables (14-42/44) support these decodes. This audit does
**not** verify guarded extent, full control flow, allocator/lifetime semantics, capacity or OneShot mode naming.
Read-only reproduction uses `container.sections` → `extract.classify` → `elz.depack_section` →
`LoadedMemory.from_stream` → `decode_confident_loaded` with `.venv/bin/python -B`; no generated core is required.

The loader's 400 MiB offset bound and the manual's documented project capacity are distinct observations;
neither proves an SDK allocation or a safe borrowed-view lifetime. Release/reset evidence makes lifecycle a
concrete dependency rather than an assumption that an address stays valid.

## DN2 comparison: dependencies, not playback feasibility from scratch

**[D]** Read-only `Image.match` and NetworkX comparisons found no relocation-hash matches in DN2 for the listed
DT2 slot/setup/page/renderer functions. This is **not evidence that functional equivalents are absent**.
The exact-hash common pair DT2 `0x1cdd8b` / DN2 `0xb8251b` is a downstream per-voice hook, not a sample reader;
DN2 engine `0x1c8ef1` reaches it.

The existing DN2 adapter already plays samples through its voice seam, private source acquisition and float32 L2
bank. Preserve that working path. **[O]** Larger owned backing and delivery remain missing contracts, distinct
from playback. Investigate those gaps against the concrete DT2 dependency list, rather than restarting from a
claim that DN2 sampling is infeasible. External-looking addresses, a CF cave or module arena alone establish
neither owned DSP memory nor a delivery path; do not assume DT2's stock pool or LP0 exists on DN2.

## Executed follow-up

[Finding 82](82-stock-sampler-lifetime.md) records the scoped lifecycle discovery, image identities, retained
captures and stock-only reproduction status. OneShot selection, stored slot binding, verified historical ten-second fixture input
and a passing active replacement reproduction on the original unpatched DT2 1.17 image are now recorded.
Start/end/reverse, removal and project reload
remain open; the full lifecycle is not complete. The result requires explicit retirement before overwrite,
including external replacement/reset paths, rather than treating a metadata acknowledgement as a barrier.

## Bounded next proof, before implementation

- [ ] Inventory stereo format/channel layout, encoding/rate support, frame/length fields and long-sample limits
  on both current images; do not use the historical ten-second fixture as a product cap.
- [ ] Establish DT2 stereo pool accounting: bytes per frame, guards/alignment, actual usable capacity and
  deduplicated slot/reference ownership. The 400 MiB bound does not prove stereo product capacity.
- [ ] Establish DN2 backing ownership, usable capacity, delivery and reset/coexistence bounds for the same
  shared project pool across 16 tracks, without fixed per-track partitions.

- [x] Select stock DT2 OneShot on OS 1.17 and capture selected slot → stored voice binding with actual PCM;
  active-note and image-specific replacement evidence is separated in finding 82.
- [ ] Capture start/end/reverse behavior and sample-change/project-reload transitions, including old-note stop behavior.
- [ ] Use those observations to specify adapter readiness, retention, invalidation and reader-quiescence appropriate
  to the actual stock contract; transport ACK alone is not that barrier.
- [ ] Specify a shared packed-PCM16 reader/cursor/region contract independently, then prove it numerically before adoption.
- [ ] Map each DT2 dependency onto the already working DN2 seam; bound investigation of missing larger backing
  ownership and delivery. Decide the next probe only from those concrete gaps.

**[O]** After the proof design is agreed, reuse the existing TypeScript regression session APIs in
`sample-playback.test.ts` under `tests/regression/runtime/` for scheduled input, DSP peeks (including unknown-word
counts) and PCM capture. Keep a selected-slot change distinct from REPLACE/removal of a referenced slot and
project reset/reload: selection may retain the old allocation, whereas the latter operations can reuse its extent.
Use distinct deterministic PCM fixtures to expose stale old reads versus new data. Capture the CF slot map/block
reference count, DSP metadata and active voice pointer before and after each transition. Hearing the new sample
or observing READY alone does not prove reader quiescence. This note adds no scenario or test implementation.

Full stereo is now a product requirement; no stereo implementation, throughput claim, new pool ownership or
hardware validation is established by this note. Current immutable-bank acceptance remains unchanged.

## Reproduction

Use the read-only firmware query CLI, with existing current-image inputs. For a function dossier:

```sh
uv run python -m tools.coldfire.fw dossier 0x4015481c -i dt2-1.17 --decomp 80
```

Repeat the dossier for the CF entries in the table. The existing `batch` command accepts one such command per
line without the Python/module prefix; batch the entries to share query setup. DSP listings use the same image:

```sh
uv run python -m tools.coldfire.fw disasm 0x1c4e91 -i dt2-1.17 --cpu sharc --max 16
```

Do not implicitly rebuild shared databases. Image-match results establish only the stated hash comparison;
retain the distinction between database observations, historical semantics and a future controlled runtime proof.


## Project invalidation follow-up

**[D]** Current DT2 1.17 database dossiers retain the slot/path loader at
`0x4015481c`, slot map `0x405b5368`, and loaded-block vector globals
`0x405a0f64/68/6c`. Release `0x401543a2` removes a slot mapping and only
releases/coalesces the associated block when its record reference count at
`+0x30` is zero. `ReloadAllSamples` at `0x4004e8be` can queue the reload
path through `0x40154634`, which clears the 1024 slot mappings via
`0x40153ec6`, resets the vector and emits the stock sample event.
These are concrete invalidation paths for any proposed borrowed view.

**[C]** Candidate `0x401363d0` is a byte-fill helper (destination, byte value,
length), not a sample allocator. Its repeated-byte word fill and tail stores
do not supply an independently owned DSP extent.

**[O]** The stock loader remains useful evidence, but an adapter still needs
an ownership contract and a reset/replacement fence before reusing its pool.
The dossier observations do not establish either contract.


## Historical explicit-tail reservation candidate

The tail-reservation recommendation is withdrawn under the current requirements above. The following
allocator observations are retained as historical evidence; they do not authorize a reservation patch.

**[D]** A bounded DT2 1.17 loader follow-up distinguishes the initial rounded
block-size check at `0x40154aea` from a shared pool boundary. The loader also
reuses gaps and grows/merges records. Helper `0x40154682`, called by
`0x40154770` and the loader at `0x40154c10`, derives the next offset from the
last vector record (or `0x400` for an empty vector) and compares the proposed
end against an exclusive `0x19000401` bound in the decompiler; original bytes
below establish the encoded comparison as unsigned `> 0x19000400`.
Its decompiled return encoding is not a complete ABI
proof. The vector's storage-end pointer `0x405a0f6c` concerns metadata capacity,
not an independently owned sample tail.

**[O]** Lowering a single 400-MiB literal does not establish a carveout.
A reservation must constrain initial allocation, append/growth, gap reuse and
reset/reinitialization consistently. The current pass identifies those enforcement
questions; it has not implemented or validated a smaller stock pool. Borrowing a
stock allocation remains another route only with a fence before reclamation.


## Independently audited pool-boundary and writer follow-up

**[D]** A second agent extracted DT2 1.17 original main-section bytes and checked
both bounds. Rounded size is `(size+0x200f)&0xffffe000` at `0x40154aca/ade`.
The limit at `0x40154aea` is `0x19000000`, with signed BGT rejection at
`0x40154af0`. Append candidate end is compared against `0x19000400` at
`0x4015470a`, with unsigned BHI rejection at `0x40154710`; equality succeeds.
The first allocation starts at `0x400`. For a proposed end E, the corresponding
limits are E−0x400 and E. Subtracting the same delta from the original limits
preserves that relationship.

The bounded record-writer family includes loader emplace calls at `0x40154b56`
and `0x40154d1c`, insertion at `0x4015500a`, and inline construction/end advance
at `0x40154fd8/fe2`. Gap reuse `0x40154c28..30` allocates inside an existing gap.
Release `0x40154458/62/486/4a2` shifts records, shrinks the vector and merges freed
space into the next gap. Reset `0x40154662` sets end to begin. An absolute-reference
scan also found vector-global initialization at `0x401e2b8e/94/9a` outside indexed
function boundaries. These positive paths support a conditional extent invariant;
they are not an exclusion proof for arbitrary computed pointers.

**[O]** Input-domain evidence remains material: a size of `0x7fffdff1` rounds to
`0x80000000` and passes the signed initial comparison. Rounding and append addition
also need nonwrapping operands. No upstream restriction excluding these cases was
established in this bounded audit. Changing two literals alone is therefore not
proven safe for all accepted inputs. No reservation patch was implemented or
approved. The next DT2 adapter question is a fence before stock reclamation or
overwrite, using the shared SDK reader-retirement contract.


## Before-reclaim adapter seams

**[D]** Independent original-byte checks establish that release `0x401543a2`
removes a slot-reference node at `0x4015443a`, decrements record count at
`0x40154448`, and can remove/coalesce records at `0x40154458..a2`, before its
header-clear call at `0x401544de`. A custom-reader fence must precede reclamation;
intercepting header clear alone is too late. Reset `0x40154634` first clears a
header at `0x40154640`, destroys records at `0x4015465c`, then resets vector end
at `0x40154662`: all borrowed SDK bindings must drain before its first clear.
Loader replacement calls release at `0x401548f4`, before deduplication/allocation
and first PCM sends at `0x40154dce/ee`. These are ordering facts, not a callable
production continuation interface.

Reload dispatch checks SampleLoaderBgWorker identity and queues work when invoked
off-worker, but this does not establish that every release caller is worker-only.
The queue unlock at `0x400f0b12` precedes job callback `0x400f0b3c`; queue protection
is not a global allocator lifetime lock. The separate queued page sender
`0x40153f34` has a positive preview producer at `0x4015431e`, using fixed preview
buffers, rather than evidence that general-loader pages share that route.

**[O]** The current private CLOSE request progresses in the UI-loop tick. A
synchronous wait in a UI-originated release/reset could prevent both sending and
completion polling. Shared-pool integration therefore needs an asynchronous
suspend/resume or a separately proved progress context, with exact DRAINED before
mutation. On failure retain the backing and suspend or abort upstream; returning
early from a void release/reset function is not enough to stop its caller.
A genuine balanced stock slot-reference pin could prevent ordinary last-slot
release, but is not an active-reader pin and does not survive project reset's
vector destruction. Neither production adapter route is implemented by this pass.

**[D]** The immediate caller audit also resolves two misleading candidates.
ProjectSettings virtual `+0x28` resolves through vtable `0x401f4e90` to
`0x401a96b8`, a field getter with no calls. Removal wrapper `0x4004b794` clears
the project slot at `0x4004b7f0`, calls release at `0x4004b7f8`, then invokes
virtual `+0x10` notification at `0x4004b81a`. Reload's worker path calls the
getter at `0x4004e962`, stores a flag, then calls reset at `0x4004e976`.
The immediate loader predecessor `0x4015b616` only compares a four-word sample
key, and busy-scope exit `0x40153896` only clears its flag. None of these
inspected calls establishes a DSP drain acknowledgement. This bounded result
does not exclude a barrier higher in the call chain or stock DSP slot validation.

### Header receiver and immediate cached reader

**[D]** A further original-byte audit follows marker `-1` through
`0x1c7e35/38/39` to header handling at `0x1c7e83`. **[C]** Helper
`0x1c400f` copies five sample guard shortwords; it is not slot invalidation.
The actual slot update is `0x1c3fe5`, called at `0x1c7eb7`: it checks slot
`< 0x401`, computes `slot*0x14 + 0x257810`, and writes five record fields
at `0x1c4001..08`. This leaf does not walk voices or clear cached voice pointers.
The following path constructs a receive descriptor and calls `0x1c834a`;
its nested driver return has not been established as a reader-drain grant.

The immediate renderer consumes cached voice state: `0x1c504a` loads I0 through
I4/M5, `0x1c50a4` sets I4 from I0, and `0x1c50b4` reads the first PCM tap.
The inspected non-Stretch dispatch at `0x1c6afd/0x1c6b00` passes the voice-state
pointer to `0x1c4ecf`; entry-to-first-tap does not resolve the slot again.
Earlier dispatcher/state updates remain outside this bounded proof.

CF writers `0x400ccf04/0x400ccf90` set control `0x80`, poll status bit 0 before
eight port writes, then return without reading a semantic response tuple.
The enclosing sender's delay/mutex release is not a proven header-applied or
old-readers-drained acknowledgement. These observations strengthen the need for
an explicit pre-reclaim adapter fence; they do not prove that no broader stock
coordination exists. The original DT2 1.17 image hashes match the audit above.

## Stereo storage and length accounting

**[D]** A bounded original-byte audit of current DT2 1.17 resolves the file-to-pool
layout. Native files contain interleaved PCM16, while the stock DSP pool stores
stereo in two channel planes. Split helper `0x40153884..8e` separates successive
16-bit values, preserving first/second channel order. Loader `0x40154dc0..de` and
`0x40154e60..86` send each chunk's two halves to the corresponding planes.
The source flag is native-header byte 1, tested for equality to 1 at
`0x40154da4..aa`; the deduplicated path uses the saved result in `0x405ba368[slot]`.

The current native writer `0x40153c70` sets header byte 1, total PCM byte count at
byte 4, and 48,000 at byte 8 (`0x40153d12`, `0x40153d0e`, `0x40153d20`). It sizes
the file as PCM bytes plus `0x50` and writes a sixteen-byte trailer. For a valid
canonical N-frame, C-channel PCM16 file, with C=1 or 2:

`P = 2*C*N`, `F = P+80`, `A = align_up(P+96,8192)`.

These are distinct quantities, not interchangeable frame counts:

| Field | Meaning | Current-image evidence |
| --- | --- | --- |
| File header byte +4 | Total PCM bytes P | Writer `0x40153cb2..d0`, `0x40153d0e` |
| Allocation record byte +0x14 | Rounded extent A | Loader `0x40154aca/ade`, `0x401549fe` |
| CF header packet byte +0x10 | Per-channel bytes: P mono, P/2 stereo | Builders `0x40153e90`, `0x40153e44..46` |
| DSP slot record byte +8 | Same per-channel byte count | Receiver `0x1c7e90/0x1c7eb3`, store `0x1c4006` |
| Lookup result | Per-channel byte count /2 = N frames | `0x1c3fbf`, `0x1c3fc1`; M7=-1 initialized at `0x1c0f3c` |

For pool block offset B, stereo PCM starts at `B+32` and `B+A/2+32`; mono starts
at `B+64`, with the same pointer supplied for both channels. Packet pointers
subtract `0x400` before the established DSP rebase. Allocation rounding is based
on full file size, not just the PCM payload. An SDK adapter must not expose this
stock backing as interleaved stereo or treat any byte-length field as frames.

Audit hashes match the original SYX/section-7 identities above; main section 3 is
3,275,616 bytes at `0x40000400`, SHA-256
`a1e7b657b705eba1a19d81c33c1e11ba9c409816447ad74005d7bbf36da6d964`.
**[O]** Canonical accounting does not establish validation of arbitrary input:
header PCM length versus actual file size/allocation remains the next bounded
check. Ownership, usable SDK capacity, reader retirement and supported input-rate
bounds remain separate questions. No new emulator or hardware run was performed.

## Header validation and stable source requirements

**[D]** Original DT2 1.17 main bytes (same hashes above) distinguish the loader's
successful storage read from validated PCM geometry. Storage size F is read at
`0x40154ab0` and rounded before the first header read at `0x40154d8a`. The next
path copies 64 header bytes without a preceding read-count predicate in that
slice. Initial page sends precede cumulative-read checks at `0x40154e06/e0a`;
final equality at `0x40154eba/ebe` compares total read bytes with F. Header P is
passed separately to the mono/stereo header builders at `0x40154eec/0x40154ed0`.
No P-versus-F check was established in this bounded loader path. This does not
exclude validation in an upstream importer.

When the pool is nonempty, allocation uses metadata size M from key+8, loaded at
`0x40154ba4`; the initial rounded F still controls stereo displacement at
`0x40154dd2/dd4` and `0x40154df6/dfc`. Both metadata helper `0x4015ae6a` and path
size helper `0x4015bd38` obtain filesystem node+4 at separate times. Individual
reads are mutex-protected; a stable snapshot across lookup, allocation and all
reads is not established. No M==F comparison was identified in these paths.

**[O]** Runtime-originated loading therefore needs preflight before side effects:
stable identity/version, full header, supported flags/rate, nonzero frame count,
P divisible by twice the channel count, F=P+80, checked rounding/addition, and
agreement with the resulting allocation and plane spans. Borrowing an already
loaded asset needs validation of the resulting view but cannot undo earlier stock
writes. Neither check establishes retention or reader retirement.

## Pool writes are interrupt-reachable

**[D]** Original section-7 bytes establish a positive interrupt route, not a
render-boundary task handoff. Setup `0x1c88f0/f3/f6` registers handler `0x1c8705`
for SID `0x51` with context `0x269468`. Registration through `0xb8afa2`,
`0x1c066c` and `0xb89266` installs the trampoline and tables used by the SECI
vector at loader `0x28240168`, targeting `0x1c0b7b`. Handler completion reaches
`0x1c85ba`, which invokes registered receiver `0x1c7e1f` inline with event 4
at `0x1c86a5..b0`. This receiver directly calls the page/guard copy family.

DMA targets staging, not final sample backing: descriptor `0x268220` contains
ADDRSTART `0x268240`, count `0x401`, XMOD 4. Submission converts the source address
to byte alias `0x28268240` and programs the DMA registers at `0xb8c6c2/cc/d6`.
The CPU then writes the pool:

| Writer | Bounded footprint / ordering |
| --- | --- |
| `0x1c403c` page copy | Checks page <`0x19200`, copies 4096 bytes to `0x8422b9c8 + page*4096`; stores at `0x1c4056/60/6a` |
| `0x1c400f` guard copy | Five 16-bit stores at `0x1c4025/29/2d/31/35` to base+start+per-channel-byte-length; no extent check in this leaf |
| `0x1c3fe5` metadata update | Runs after both guard calls at `0x1c7e89/96`; does not precede their PCM writes |

**[O]** Closing a binding immediately before a copy is insufficient if a reader
was interrupted after acquiring its pointer. On interrupt return it could resume
reading overwritten memory. Actual live interrupt masks/priorities remain unproved;
this is an IRQ-reachable path, not an observed preemption. A DSP-side solution
needs a proven read exclusion scope or deferred final writes with staging/rearm
ownership. The CF asynchronous close/drain route remains another option. This
bounded writer inventory is not an exclusion proof for every computed writer.


## Calculable pre-store seam, without successor publication

**[D]** A bounded original-byte audit of DT2 1.17 section 7 (SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`)
confirms event 4 reaches `0x1c7e35`. Its page call at `0x1c7e79` enters
`0x1c403c`, checks unsigned page p < `0x19200` (hexadecimal), and writes
`[0x8422b9c8+(p<<12), 0x8422b9c8+(p<<12)+4096)` at
`0x1c4056/60/6a`. The header marker −1 path calls the guard helper at
`0x1c7e89/96`, writing `[base+start+length, base+start+length+10)` through
`0x1c4025/29/2d/31/35`, before slot publication `0x1c7eb7 → 0x1c3fe5`.

The page path neither invalidates the slot nor records loading progress; old
metadata remains readable. The slot publisher has no generation or READY-last
protocol and validates the slot only after the guard writes. The pre-store spans
therefore supply a calculable interception seam, not successor identity,
completion or reader admission. No production memory policy is selected.


**[D]** A bounded DT2 1.17 original-MAIN audit (SHA-256
`a1e7b657b705eba1a19d81c33c1e11ba9c409816447ad74005d7bbf36da6d964`)
identifies the general loader's direct page/header sender
`0x40153a10 → 0x400cd79c`, with header calls `0x40153e54/eae`; it does not use
queued-preview `0x40153f34`. The sender takes per-packet mutex `0x42948b14`,
writes the tag and 1,024 words to MMIO, waits through `0x40136544(100)` and
unlocks. This is more than enqueueing, but supplies no DSP-applied identity tuple.
The new-load byte-count/file-size equality at `0x40154eba/ebe` gates header calls
`0x40154ede/ef6`; it does not establish geometry or stable successor identity.
Reload worker `0x4004e8be` does not prove whole-load serialization across other
callers `0x4004b502/60a`. Header return is not proof of publication or admission;
no production memory policy follows from this bounded sender audit.


**[D]** In the same original DT2 1.17 MAIN audit, this bounded reference-tree
family stores unique slot IDs, not opaque leases: tree at record+`0x1c`, sentinel
+`0x20`, count +`0x30`, `0x14`-byte nodes with key +`0x10`. Deduplicating insert
`0x401e24b0/0x401e4352` precedes inline count/slot-array `0x405b5368[slot]`
updates and DSP publication. Release `0x401543a2` always clears the slot/header
through `0x40153ec6` after erase/free/decrement. Rebuild `0x4015394a` uses every
node key as an unchecked slot-array index at `0x4015399c/a0`: a fake key is unsafe,
and a duplicate existing key adds no reference. Record move `0x401e237a` also
moves the tree/sentinel, so a raw record pointer is not stable. No balanced
slot-independent pin is established in this family; this is not a production
ownership decision or an exclusion proof for every holder.


## DT2 file-source consistency, separate from pool references

**[D]** A bounded original DT2 1.17 section 3 audit (SHA-256
`a1e7b657b705eba1a19d81c33c1e11ba9c409816447ad74005d7bbf36da6d964`)
checked 475 instructions across nine bodies, then 382 additional output-chain
instructions and four sites. This is one original-byte audit, not [V]. The
[path anchors](../../devices/fw/storage.toml) resolve open `0x40158902`, read
`0x4015860a`, close `0x401588c0` and size `0x4015bcfe`. The four-word handle holds
entry ID, position, write mode and open flag. Read body `0x4015824e` relooks up
metadata/extents on each read under mutex `0x46f4deb0`, releasing its per-read
references afterward. The API therefore supplies no immutable source lease for
[the runtime source adapter](../../modules/core/runtime/src/rs/src/sample_source.rs).

Open marking `0x4015bf40` sets an ID bit in `0x46f4ded0`; close `0x4015bf68`
unconditionally clears it. Check `0x4015bf92` gates delete `0x40156e00` at
`0x40156e18` and path move `0x40156b00` through call `0x40156b3a`. Open is not
exclusive: the read/existing-write branches do not reject an already-open bit;
`0x401589cc/ce/d4` retain ID, set mode and mark open. Create `0x4015ba88` allocates
a new ID. A bit alone consequently does not establish balanced multi-handle
ownership or prevent every writer from using an existing ID.

The bounded RPC output chain is `0x40129424 → 0x401283e0 → 0x40128448 →
0x4015946c → 0x40159506`, reaching File constructor `0x401590b2` in mode 1.
Its delete attempt `0x4015910e → 0x40158e30 → 0x40157078 → 0x40156e94 →
0x40156e00` ignores the delete result: the call at `0x40159114` overwrites it.
It then opens mode `w` at `0x4015915c`; an existing ID can remain after a busy
failed delete. Stream writes follow `0x40158c7a → 0x40158c18 → 0x4015866a`.

**[O]** Outer RPC admission is not fully audited. These source-level facts do not
establish an observed race or acceptance of every Transfer request. Per-read
locking and open marking do not prove same-length replacement detection or
consistent multi-read content. Next audit the mutation-under-mutex seams as
possible points to invalidate a partial load on change; no hook, public ABI,
product limit or production policy is selected. DN2 snapshot generation/SHA is
already recorded in [the source inventory](../plans/dsp-sdk-and-sampler.md);
it is a separate source path, not a lease claim for this DT2 filesystem or SHARC
pool-reference family.

### Mutation seams under the filesystem mutex

**[D]** In the same original section 3 audit, 18 additional instruction heads
were checked. Write `0x401586ae` acquires mutex `0x46f4deb0`; `0x401586b4`
loads file ID from handle word 0 into D7. The first potential extent mutation is
`0x401586e0 → 0x40158448`, whose allocation arms at `0x401584f0/58596` reach
`0x401581dc`. Partial payload uses memcpy at `0x4015878a`; full blocks instead
follow `0x40158758 → 0x40155e84 → 0x40155ed8 → 0x4012e39c` (block write),
bypassing that memcpy. Length is stored at `0x401587bc`; unlock is `0x401587f8`.

Delete `0x40156e00` does not acquire the mutex itself. Known path wrappers
`0x401570a6..570bc`, handle wrapper `0x401570cc` and recursive `0x40157192`
hold the same mutex. File ID is the second argument, in D2 at `0x40156e08`.
Directory unlink `0x40156e52 → 0x40156bf6` stores at `0x40156c5a/60`;
reclaim follows `0x40156e6e → 0x4015614e`. Visible additional paths include
reserve `0x40158810 → 0x40158448` and recursive alternate reclaim
`0x4015711a → 0x4015614e`.

**[O]** These are candidate interception seams, not complete writer coverage.
Create/ID reuse, reset and other block writers remain open. A proposed generation
for the selected file could reject an unpublished load on mutation only if every
mutation/reclaim/reset path is covered and counter wrap fails closed. No such
mechanism, public API or product policy is implemented; neither file immutability
nor DSP pool ownership follows from this audit.

### Reserve and alternate reclaim bypass write/delete hooks

**[D]** A further bounded DT2 1.17 audit checked 822 original section-3 encodings
(SHA-256 `a1e7b657b705eba1a19d81c33c1e11ba9c409816447ad74005d7bbf36da6d964`).
Hooks only at write `0x4015866a` and delete `0x40156e00` would miss both families
below. This is CF source-consistency evidence, separate from DSP pool ownership.

Reserve `0x40158810` keeps the handle in A2, locks mutex `0x46f4deb0` at
`0x40158836`, pushes handle word 0 (file ID) at `0x40158846`, and unlocks at
`0x40158854`. Its allocation chain is `0x40158448 → 0x401581dc → 0x40155974`;
the first bitmap set is `0x401559fe`, persisted at `0x40155a66`. Extent-node
writes are `0x40157c98/9a/9e`, with count at `0x40157ca8`. Immediate callers
are `0x40153cde` and `0x40158bb8` (the latter uses object+8 as its handle).
This bypasses the old write entry and performs no payload write itself.

Alternate reclaim `0x4015711a` has file ID in D3 at `0x4015711e`, looks up the
node and passes node plus ID at `0x40157130 → 0x4015614e`; it takes no lock
itself. First mutation `0x40156160` decrements the node+2 count. On zero,
`0x401580ae → 0x40155ae4` clears allocation bits, persists at `0x40155b36`
and reaches block-range work `0x40155b58`. Tail `0x4015bb7e` clears the ID
allocation bitmap at `0x4015bbc6`, persisted at `0x4015bc00`.

Known parents hold the same filesystem mutex: `0x40157192` acquires at
`0x401571c8`, reclaims at `0x4015724c/52` and releases at `0x4015725e`.
This recursive parent first calls the old delete path for one ID, then reclaims
both child and parent IDs, so invalidating only the delete argument would be
insufficient. Rollback parent
`0x4015746e` acquires at `0x401574a4`, reclaims newly allocated ID D2 at
`0x40157586` and releases at `0x401575ac`, bypassing delete.

**[O]** File identity is available before these first mutations; lower bitmap
helpers no longer receive it. The two-family audit is complete within this scope,
not complete mutation coverage. Create/ID reuse, reset/remount and other block
writers remain open before any selected-file generation scheme could be complete.
No hook implementation, public contract or production memory policy is selected.

### Create/ID reuse exposes identity before mutation, not a complete generation

**[D]** A bounded DT2 1.17 audit checked 508 original section-3 encodings against
SHA-256 `a1e7b657b705eba1a19d81c33c1e11ba9c409816447ad74005d7bbf36da6d964`.
Allocator `0x4015ba88` scans bitmap `0x46f58ed0`. Selected ID D5 is computed at
`0x4015bab6..babc`, before the first bitmap store `0x4015bac6`; persistence is
`0x4015bb0c`. It clears the reused `0x80`-byte node at `0x4015bb20`, sets
node+1 to 2 at `0x4015bb26`, and initializes global `0x4099d68c` to 1 when zero, then always increments it
at `0x4015bb2e..46` (the first assigned value after zero is 2). The value is stored at node+`0x10` at
`0x4015bb4c`. This is incarnation-like metadata, not a fail-closed generation:
no wrap rejection was found in this body.

The allocator takes no lock itself. All four recorded call sites are inside
filesystem mutex `0x46f4deb0` scopes: open `0x40158902` allocates at `0x40158a28`
between lock `0x4015894e` and unlock `0x40158a84`; directory create `0x4015746e`
allocates at `0x4015753e` between `0x401574a4` and `0x401575ac`; reset-like
`0x4015a440` allocates at `0x4015a5d0/648` between `0x4015a452` and
`0x4015a694`. This bounds the recorded callers, not every computed entry.

Open receives the new ID in D2 at `0x40158a30`, calls link helper `0x40156bd8`
at `0x40158a4e` with a separate parent ID and filename, then publishes handle
word 0 at `0x40158a5e`. Link failure directly follows
`0x40158a78 → 0x4015bb7e`, bypassing delete/reclaim wrappers. Identity is available
before first mutation, but existing wrapper-only hooks would still be insufficient.

**[O]** This bounded create/ID-reuse slice is complete. The lifecycle and I/O subsections
below record the completed bounded follow-ups; directory-link, init/indirect and
error/completion coverage remain open. No complete
mutation-generation scheme, hook implementation or public API is selected, and
this CF source evidence establishes no DSP pool ownership.

### Format, mount and init are distinct global lifecycle boundaries

**[D]** A bounded DT2 1.17 audit checked 622 original section-3 encodings across
19 bodies and two windows, against the section-3 SHA-256 above. Format
`0x4015a440` takes the FS mutex at `0x4015a452`; open-bit scan `0x4015bfc4`
at `0x4015a458` rejects a nonzero result. Accepted boundary `0x4015a478`
precedes mounted flag `0x44f2bd68` clearing at `0x4015a48a`, erase
`0x4012e0ce` at `0x4015a490`, and 512-byte header zeroing at `0x4015a4a0`.

Mount `0x4015a72c` locks at `0x4015a734`. Boundary `0x4015a73a` precedes
flag clearing `0x4015a74a` and header replacement at `0x46f4dcb0` through
`0x4015a750`. It checks magic/checksum/version 3 or 4, calls shared init
`0x4015a400` with flags at `0x4015a7b0`, then sets mounted=1 at `0x4015a7cc`
and unlocks at `0x4015a7de`. No local open-handle check was found. Separate
init `0x4015a716` resets the mutex through `0x4000157c` at `0x4015a71c`, then
clears the mounted flag at `0x4015a722`, without taking that mutex. Its mutex
owner store was absent from the decompilation; waiter-word stores were present. Shared `0x4015a400`
is therefore too late to serve as the sole global-invalidation boundary.

Open bits occupy `[0x46f4ded0,0x46f58ed0)`; ID bitmap starts at `0x46f58ed0`
with length `0xb000`. Format zeroes it through `0x4015b7cc` at `0x4015a512`
and reserves IDs 0/1. Mount's `0x4015a400 → 0x4015b804` reloads it at
`0x4015b816`; metadata reset is `0x4015b74c`. Block bitmap `0x46f39ca0`,
length `0x14010`, follows format `0x40155900` / mount `0x40155954`.
Shared init also recreates global directory handle `0x46f4debc` with ID 2 through
`0x40157338`/store `0x4015733c`, another store missed by decompilation.
Format ignores the shared-init return at `0x4015a634`, sets mounted=1 at
`0x4015a63e`, optionally creates the incoming directory, then unlocks at
`0x4015a694`. Mounted 0→1 is not a generation; no existing global epoch is
established.

Immediate format wrapper `0x4015a700` passes argument 1. Startup `0x400cc9c8`,
when `0x42940a48` is zero, calls init at `0x400ccd4a`, then uses bit `0x10` of
`0x4029eef0` to choose format `0x400ccd5a` or mount(0) `0x400ccd64`.
Console `0x400caff0` recognizes `#FORMAT_FS` at `0x40241da0` and calls at
`0x400cbedc` when unmounted or overridden. Callback `0x4009eb88` calls at
`0x4009ec5a` when object byte +9 is nonzero; its +8 indirect branch is unaudited.
Shared init `0x4015a400` with argument 0 reaches factory helper `0x4002cd6a`;
downstream work is unexpanded. **[O]** Runtime/indirect init triggers, balanced open ownership and
complete lifecycle coverage remain open. DSP pool reset `0x40154634` is separate.

### Successful filesystem block I/O waits; error draining remains open

**[D]** A separate audit checked 789 original section-3 encodings. Successful
block read `0x4012e1b6` and write `0x4012e39c` hold mutex `0x47de3cac` and
wait for three events through `0x4000141a` (blocking TRAP 0), rather than only
enqueueing work. Write waits at `0x4012e53c`, `0x4012e550`, `0x4012e562`,
with intervening status checks. Read waits at `0x4012e334`, `0x4012e348`,
`0x4012e350`, then performs the bounce copy at `0x4012e370`.

After partial-payload memcpy at `0x4015878a`, the path through `0x40158796`
reaches `0x40155c34`. Its disk-backed branch follows `0x40155cbc → 0x4012e39c`;
its separate RAM-backed branch copies directly inside `0x40155c34`. Cache release
`0x4015879e` precedes FS mutex unlock `0x401587f8`;
no deferred flush was found here. Read-cache miss `0x40155cd0 → 0x40155e70 →
0x4012e1b6` precedes `0x4015824e`'s user copy and unlock at `0x40158654`.

**[O]** Early error returns skip waits, and the filesystem wrappers ignore block
status. Success-path completion therefore supplies no error-path DMA quiescence
or reliable failure-reporting claim. The bounded error audit below narrows those
paths; source consistency, full mutation/lifecycle coverage and any fail-closed
generation mechanism remain unproved. No hook, public API or DSP backing policy is selected.


### Error ISR signaling does not establish transfer draining

**[D]** A bounded DT2 1.17 audit checked 416 original section-3 encodings and
reference-scanned `0x4012d500..0x4012e5a0` with 100% decoded-byte coverage.
In ISR `0x4012d5ba`, command-timeout IRQSTAT bit 16 is acknowledged at
`0x4012d5d8`, sets `0x44e3feac=-1` at `0x4012d5e0`, and signals an event at
`0x4012d5e6`. CRC bit 21 is acknowledged at `0x4012d62c`, sets the data-error
flag at `0x4012d634` and signals at `0x4012d63a`; data-timeout bit 20 follows
`0x4012d658/660/666`. These arms contain no DMA disable/reset, ACTIVE/DONE poll,
abort or drain. Signal `0x4000155c → 0x400011ee` only updates event/runnable state.

Channel-59 ISR `0x4012d6a0` writes EDMA_CINT=`0x3b` at `0x4012d6b0`, then
signals at `0x4012d6b6`. Interrupt clearing is not transfer stopping. The public
MCF54418 Reference Manual, catalogued in
[the ColdFire source notes](../refs/MCF5441X-notes.md), describes CINT at printed
p. 19-17 (local PDF page 0377). CSR=`0x000a` selects major-loop interrupt and
DREQ request disable at count zero, pp. 19-31–32 (pages 0391–0392), not an
error-path abort. IRQSTAT error definitions are at p. 25-24 (page 0640).

Command-error paths in both block operations return without waiting for the DMA
event. Write data error returns −2 before that wait; read data error is checked
after the DMA gate. Common `0x401345a2` only unlocks `0x44e4ae98`, followed by
block-mutex `0x47de3cac` unlock, without cancellation. D0 errors remain observable
at read exit `0x4012e390` / write exit `0x4012e592`, though FS wrappers ignore
them. Write also ignores the preliminary command return at `0x4012e456`.
Error globals are not sticky: new requests overwrite them, and independently
checked ISR bits allow a later command-complete arm to write zero. Simultaneous
error/completion bits therefore need care.

**[O]** The bounded error-path audit is complete, not controller-wide or physical
DMA-quiescence proof. The pointer audit below resolves the full-block filesystem
read destination as stock cache rather than runtime caller storage; no late write
has been observed. Controller-state proof or conservative uncertain-completion
handling remains unresolved. No hook, source-generation scheme, public API or
production backing policy is selected.

### Runtime read destination is copied from stock cache, not a DMA target

**[D]** A bounded audit checked 398 original DT2 1.17 section-3 encodings.
Runtime [source reads](../../modules/core/runtime/src/rs/src/sample_source.rs)
pass `dst+done` in chunks of at most 4,096 bytes to `0x4015860a`.
`0x40158644 → 0x4015824e` passes that caller destination as the fourth argument.
The path `0x401582c0 → 0x40155ef2 → 0x40155cd0` selects stock cache;
cache miss `0x40155e70 → 0x4012e1b6` uses destination
`0x405d5770 + cache_slot_offset`, optionally through block bounce storage
`0x4fe69300`, rather than the caller pointer. At `0x401582fe`, memcpy copies
cache+offset into the caller using the computed/clamped chunk, the minimum of
request remaining, block remaining and bytes to EOF; `0x40158306` then releases
it. No full-block direct-to-caller branch was found: even `0x8000`-byte requests
use this copy path. The final one-byte stack probe uses the same reader, but at
EOF it returns before cache lookup or copying.

This path supplies no evidence that DMA retains the runtime arena/input pointer,
no observed after-free write, and no reason to retain that input buffer solely
for this filesystem DMA route. It does not establish safety for other DMA paths.

**[D/O]** `0x40155cd0` ignores the block-read error and still returns the cache
pointer. Stale, incomplete or potentially changing stock-cache contents can
therefore be copied with a filesystem success byte count. Neither reliable
`E_READ` propagation nor physical DMA quiescence follows. The active bounded
pointer/error audits are complete; exact controller recovery remains open.

The resulting design requirement is a lifecycle/content-change witness plus an
independently latched low-level storage fault. Filesystem byte counts and native
header validation alone are insufficient source validation. This is a requirement/
proposal, not a selected hook implementation or public ABI. Unknown completion
must not be assumed retry-safe; recovery and complete hook coverage remain open.
No source-generation scheme or production DSP backing policy is implemented.


**[O] Fault-history requirement:** a storage-fault record must not reset on a new
load. Installation must cover the cache's relevant I/O history or establish fresh,
quiescent cache initialization. Starting a clear token after an unobserved storage
failure is not evidence of cache health. The first-release policy is accepted in
[the plan](../plans/dsp-sdk-and-sampler.md); implementation/recovery mechanisms
remain unselected.

### Early patched-in BSS is a candidate boot-scoped fault record

**[D]** A bounded DT2 1.17 audit checked 709 original MAIN instruction heads and
eight reset bytes, against the section-3 SHA-256 above. The early reset route at
`0x4000053e` reaches `pk_loader`: segment copy and BSS zeroing in
[loader.s](../../modules/core/expansion/src/cf/loader.s) (source line 16) and
[the expansion manifest](../../modules/core/expansion/module.toml) (line 30)
precede the stock MAIN initialization at `0x4000045c` / BSS clear `0x400004b2`.
The stock clear covers `[0x40312000,0x47e28470)`; expansion storage lies outside
it, so the expansion loader performs its own BSS zeroing. Patchkit segment-BSS
link/apply construction supports this source-level ordering, not an already-built
storage-observer artifact.

Loader return `0x40000546` precedes cache setup and continuation through
`0x40000568 → 0x40196fda`, then scheduler/startup
`0x400cce52 → 0x400cc8e4 → 0x400cc9c8`. Storage initialization follows
`0x400ccc66 → 0x4012d718` and `0x400ccc74 → 0x4012d78e`. It installs fault ISR
`0x4012d5ba` at `0x4012d860`, configures/resets the controller at
`0x4012d8a8..d8f6`, enables IRQs at `0x4012d908`, then reaches the first
positively traced command at `0x4012d90e`. DMA59 ISR `0x4012d6a0` is installed
at `0x4012dd92` via `0x400ccc7a → 0x4012dd74`. FS init `0x400ccd4a` and
format/mount `0x400ccd5a/64` precede main OS task creation
`0x400cce4a → 0x4003324e`, targeting runtime-hook task `0x400337ba`.

**[D/O]** Existing patched-in adapter BSS is therefore a plausible boot-scoped
fault-record location before these traced storage events, unlike runtime-loaded
DKM BSS. This checked chain needs neither crash-preserved storage nor a later
clear hook for initial zeroing. Whole-bootstrap and all early-reset coverage
remain open; no storage-observer artifact or complete fault coverage exists yet.

Next is a bounded optional private DT2 patched-in storage-fault observer proof:
verify actual symbol/BSS extent, segment header and loader relocation, then bounded
native initialization/sticky-retention checks across early storage events and
controlled fault capture, including error plus command-complete in one ISR.
This is not the production sample-load gate. Source lifecycle/content witnesses,
complete capture history and production backing remain unimplemented; no public
ABI, reservation or recovery mechanism is approved by this evidence.


### Controlled actual ISR replay preserves the observed stock path

**[D]** The opt-in [Rust fixture](../../packages/patchkit/tests/storage_fault_isr.rs)
loads the exact stock DT2 1.17 `.syx` at runtime, verifies its SHA-256 and the
original six-byte hook-site hashes, then installs the actual assembled observer
only in an in-memory copy. No vendor bytes are embedded. Synthetic vector/level
setup enters with `Cpu::take_interrupt`; the actual ISR at `0x4012d5ba`, signal
helpers `0x4000155c` / `0x400011ee` on their no-waiter path, and RTE at
`0x4012d69e` execute under a 2,048-instruction ceiling. IRQSTAT is a controlled
W1C Bus test double, not a hardware-controller model.

The named test passed 36 sequence/SR combinations: eleven single-interrupt cases
plus one five-interrupt sequence at three SR values, totaling 48 interrupt
invocations per image. Cases include every audited error bit, completion-only,
simultaneous error/completion and sequential sticky union. Stock/patched
registers, SR, PC, A7, exact MMIO and nonobserver write traces, and status/signal
globals match; only the observer latch and its checked temporary stack writes are
excluded from the write comparison. See the
[adapter README](../../modules/core/adapters/storage-fault-proof/README.md) for
`DIGI_STORAGE_FAULT_ISR_SYX` and the invocation.

**[O]** The default native full fault-history check remains **failed/open** with
zero observer writes: Oracle completion does not establish stock ISR execution.
This component result proves neither real controller delivery nor DMA completion,
waiter/scheduler behavior, hardware or native-boot IRQ validation, complete early/
reset fault history, or production sample-load readiness. The
[next proof in the plan](../plans/dsp-sdk-and-sampler.md#next-proof-private-early-dt2-storage-fault-observer)
retains those boundaries and outstanding work.
