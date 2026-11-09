# DSP SDK and sampler discovery: current boundaries

2026-10-29. Target images: DT2 1.17 and DN2 1.12. Static source and database inspection; no new
emulator or hardware measurement. **[D]** means read in source/configuration or the named image's
database during this discovery; **[O]** means proposal, inference or unresolved behavior. Nothing
here is marked **[V]**. Plan: [DSP SDK and sampler](../plans/dsp-sdk-and-sampler.md).

## 1. DN2 storage is not stock sampling

Em confirms DN2 has no stock sampling. Its transferred samples live in the digikit store;
DT2 uses stock +Drive transfer (`devices/digitone-ii.toml`, `devices/digitakt-ii.toml`, `[transfer]`).
Both remain targets for general SHARC DSP analysis; DN2 sample support would be added by digikit.
Shared class names such as SampleLoaderBgWorker are discovery leads, not proof of a stock DN2 sampler.

**[D]** `packages/dksample/src/native.rs:1` defines the host-produced file: 64-byte header,
big-endian signed 16-bit PCM, interleaved for stereo, 16-byte trailer, and 48,000 Hz rate. Header
byte 1 flags stereo, bytes 4..8 encode PCM byte length, and bytes 8..12 encode rate.
The in-memory DSP representation and ownership are a separate contract.

**[D]** `modules/core/runtime/src/rs/src/store.rs:102` stages a whole file and rejects files
larger than `STAGE_BYTES`; `read_file` additionally rejects a file exceeding the caller's capacity.
The DN2 device profile sets staging to 64 KiB total file size, including its in-file header/trailer
(not the separate store metadata). The public `store_read` therefore cannot read an
arbitrarily long sample in chunks. A range reader is needed for larger resident samples as well as streaming.

## 2. Existing DN2 transport: a bounded resident-sample lead

**[D]** The runtime DSP transport is already implemented; it is not exposed as a content-module
sample API. Relevant source:

- `modules/core/adapters/dsp-link/src/cf/dsp-link.c:110`: `dl_send` validates four-byte alignment,
  nonzero length, and a destination entirely inside `_dl_area`; only one transfer can be active.
- `modules/core/adapters/dsp-link/src/dsp/dn2.s:1` and
  `devices/digitone-ii.toml:90`: 15 frame-tail words, 327,680 reserved DSP bytes.
- `modules/core/adapters/dsp-link/src/dsp/dsp-link.inc:137`: at most tail words minus three
  payload words per block, hence 12 words = 48 bytes on DN2.
- `modules/core/runtime/src/rs/src/dsp.rs:780`: module allocations account for image, BSS and
  voice/effect state; allocation is rounded to 64 bytes in the shared area.
- `modules/core/runtime/src/rs/src/dsp.rs:1127`: module image transfer precedes row publication;
  it is not an arbitrary live sample-loading API.

**[O, arithmetic ceiling]** At the configured 1,500 blocks/s, the DN2 payload ceiling is
48 x 1,500 = **72,000 bytes/s**, before acknowledgements, contention or other overhead.
Continuous uncompressed 48 kHz mono int16 PCM needs **96,000 bytes/s**. Therefore this particular
path cannot sustain that stream even at its ideal ceiling. Stereo and float32 require more bandwidth.
This does not rule out another transport or a lower-bandwidth representation; neither is established here.

**[O, capacity ceiling]** If the entire 327,680-byte area held samples, it would hold about
3.41 seconds of mono int16 or 1.71 seconds of stereo int16 at 48 kHz. Filling the same area takes
at least 4.55 seconds at that ideal payload rate, regardless of channel count. Actual available
sample duration is lower because code, state, alignment and other modules share the area.
For float32 storage these durations halve. These are bounds, not benchmarks or promised capacities.

The profile's larger L2 search range is not permission to use it as a sample pool. A larger pool needs
a separately established memory map and ownership proof. The DSP-link README also leaves hardware
cache behavior open; emulator success alone cannot settle coherency or timing.

**[O]** A minimal route is a runtime-owned allocation, bounded ColdFire reads/conversion, serialized
transfer, then publication of an immutable sample descriptor after acknowledgement. Required additions:
DSP resource ownership/accounting, range reads where needed, readiness/error state, lifetime and
replacement policy. Publishing a raw pointer before transfer completion is not acceptable.

## 3. DT2 1.17 stock sample metadata and pages

These are **[D]** current-image listing observations, not inherited verification marks from the
DT2 1.16 investigation in [finding 15](15-sharc-sample-path.md).

| Entry (short-word PC) | Observation in DT2 1.17 sharcdb |
| --- | --- |
| `0x1c3289` | Sample setup includes a call to the slot reader `0x1c3f78`. |
| `0x1c3f78`, continuation `0x1c3fa8` | Slot lookup; compares against `0x401`. The indexed path uses `slot * 0x14 + 0x257810`; address fields are rebased by `0x8422b9c8`. The stored length is shifted right once for the returned record; exact downstream channel/frame semantics still need tracing. |
| `0x1c3fe5` | Slot writer, with an indexed caller at `0x1c7e76`; writes two address fields, length, rate, and a stereo byte into 20-byte rows. |
| `0x1c7e76` | Header callback invokes `0x1c400f` for left/right offsets before slot metadata publication. Allocation/retention semantics are not established by this callback alone. |
| `0x1c403c` | Page index bounded by `0x19200`; destination arithmetic is base `0x8422b9c8 + (page << 12)`. Scalar head and tail surround 511 SIMD pairs: 1 + 2 x 511 + 1 = 1,024 32-bit words = 4,096 bytes. |

The database splits the slot reader's indexed continuation into another function. Reading only
`fw disasm 0x1c3f78` misses the valid-slot path; this is an example of why graph/function boundaries
need inspection rather than blind trust. SIMD also matters when counting page-copy words.

**[O]** A stock-slot-backed adapter could avoid duplicating DT2's sample pool. However, metadata access
does not prove the buffer remains alive during custom playback. ColdFire allocation, aliasing,
replacement and reclamation require a current-image trace. Never retain those pointers on the
strength of the slot-table read alone.

**[D]** Added machines take the digikit render path. The DT2 adapter renders a custom machine once
for its even voice and copies its mono output to the odd voice
(`modules/core/adapters/dsp-voice/src/dsp/dsp-voice.s:129`). Its SDK input does not expose the stock
sample metadata. `modules/core/adapters/machines/src/cf/dt2/machines.s:268` maps added parameters,
while indices 29/30 fall back to the clone's mapping. The historical sample-picker gap in
[finding 50](50-dt2-src-second-page.md) still needs a current behavior proof.

## 4. Machine ABI, lifecycle and stereo

**[D]** Machine v1 has one 32-float output buffer and input fields pitch, velocity and block count
(`sdk/include/dk/gen/machine.h:11`). There is no sample view, track identity, channel identity,
sample release callback, or note-off entry point in that contract. FX v1 separately has planar
stereo buffers; its stereo layout does not extend to machines automatically.

**[D]** DT2's adapter clears configured leading state bytes on note-on, calls note-on then render,
and clears the stock ACTIVE/mark on `DK_DONE` (`dsp-voice.s:1235`). Idle/fault paths can stop calling
a module. DN2 maintains per-slot phase/trig state; a rising edge arms a slot, the following block
calls note-on/render, and `DK_DONE` silences it until another edge
(`modules/core/adapters/dsp-voice/src/dsp/dsp-voice-dn2.s:272`). The stock downstream amplitude
envelope/filter chain still processes the generated buffer.

**[O]** Resource release cannot depend solely on the module voluntarily returning `DK_DONE`.
Voice stealing, external idle, faults, retriggers and type changes need adapter-owned cleanup or
a lifetime policy that avoids reclamation while any voice can reference the resource.
Session-pinned resources simplify a first proof but bound capacity and replacement behavior.
True stereo playback requires a deliberate ABI/adapter decision; silently treating the mono buffer
as stereo would be incorrect. Mono downmix, stereo rejection or a stereo ABI are explicit product choices.

## 5. General DSP extraction on both devices

**[D]** Existing SDK operations cover gain/mix, ramp, one-pole, saturation, reciprocal/division/rsqrt,
biquad, delay and half-band decimation (`sdk/dsp/README.md`). The first migration candidates are:

| Candidate | Consumer evidence | Constraint |
| --- | --- | --- |
| normalized float phase and sine polynomial | BD and PD sample loops; analog phase updates | Compare exact arithmetic and range semantics before declaring one shared operation. |
| xorshift and conversion to excitation/noise | BD render, pluck note-on | State sequence, shifts and integer-to-float scaling must stay explicit; call frequency differs. |
| integer-turn phase and quadrature sine | freqshift | Separate contract from float-phase oscillators; preserve drift and approximation properties. |
| fractional sample cursor and bounded linear read | new sampler | Separate from circular delay: endpoints, signed rate, channels and interpolation neighbors need explicit rules. |

Pluck's tuned all-pass and freqshift's polyphase network are not interchangeable merely because
both use all-pass stages. Analog's reflective folder is not the SDK saturator.

**[D]** A parent source comparison narrows the first extraction: BD and pluck both use the integer
xorshift recurrence with shifts left 13, logical right 17, left 5. Extract that integer recurrence
first. Their conversion spelling differs: BD converts then multiplies by 2^-31; pluck uses a scaled
FLOAT instruction. Preserve these conversion paths until numerical equivalence is tested. BD and PD
use the same five sine-polynomial coefficient words and multiply/add order after phase folding,
with different register assignments and surrounding modulation/warping. This supports a common
polynomial operation; it does not justify merging their complete oscillator algorithms or removing NOPs.

**[D]** Current-image catalog leads include a biquad pair (DT2 `0x1ccbd8`, DN2 `0xb81368`) and
an indexed/wavetable candidate (DT2 `0x1cc9b4`, DN2 `0xb81144`). Existing NetworkX call graphs,
built from each SQLite database, give the latter one direct callee and one reachable descendant,
`0x1c06ba`, on both images. This is structural evidence, not proof of identical semantics or a
safe callable SDK function. Table ownership, private records and absolute references must be checked.

Macros expose registers but do not automatically preserve scheduling opportunities. Block kernels
can retain optimized schedules; wrappers must reconcile their framed call ABI with the machine
adapter's entry/return ABI. Tests must distinguish instruction count from hardware cycles. The
device profiles explicitly label the 1 GHz clock an emulator assumption. See [finding 72](72-sharc-kernels-1-17.md)
and `docs/sharc/style.md` for architectural constraints and pending evidence.

## Reproduction and provenance

Use `UV_NO_SYNC=1` and a local `UV_CACHE_DIR`. The discovery worktree has separate caches; database
and manual inputs are read-only shared inputs. No DB was rebuilt.

```sh
uv run python -m tools.coldfire.fw batch - <<'Q'
disasm 0x1c3f78 -i dt2-1.17 --cpu sharc --max 0
disasm 0x1c3fa8 -i dt2-1.17 --cpu sharc --max 0
disasm 0x1c3fe5 -i dt2-1.17 --cpu sharc --max 0
disasm 0x1c7e76 -i dt2-1.17 --cpu sharc --max 0
disasm 0x1c403c -i dt2-1.17 --cpu sharc --max 0
callers 0x1c3fe5 -i dt2-1.17 --cpu sharc --max 30
fn 0x1cc9b4 -i dt2-1.17 --cpu sharc --max 20
fn 0xb81144 -i dn2-1.12 --cpu sharc --max 20
Q
```

For graph inspection, `Image.callgraph()` in `tools/sharc_dev/sharc.py` builds a NetworkX DiGraph
from the existing database; `nx.descendants` and graph successors supplied the counts above.
Do not use `sharc.load()` here: it can rebuild stale shared databases. The inspected `Image`
constructor only reads metadata, but opens a normal SQLite connection; close/replace that connection
with `sqlite3.connect(path.as_uri() + '?mode=ro', uri=True)` before graph queries. A reusable
read-only constructor is a useful small tooling follow-up, not implemented in this discovery.

Observed SHARC DB metadata (version 16):

- DT2 1.17 image SHA-256: `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`.
- DN2 1.12 image SHA-256: `336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`.

An independent evidence auditor hashed the original DT2 1.17 section-7 blob and matched the recorded
DT2 hash above. The auditor also checked the transport arithmetic, public ABI and page-copy listing.
The raw instruction bytes were not independently decoded; all observations remain [D]/[O]. DN2 raw
image provenance and instruction-byte cross-checks remain open. No resident capacity outside the configured runtime
area, safe stock-slot retention, live-load behavior, or hardware performance is proved by this pass.

## First SDK implementation checkpoint

**[D]** Exact extractions now live in `sdk/dsp/noise.inc` and `sdk/dsp/oscillator.inc`. BD, PD and
pluck use the macros. Complete assembly comparison to `177bec02` preserved executable bytes,
relocations, symbols and data: BD 1,056 PM bytes, PD 1,446, pluck 1,328. The sine polynomial is
16 instructions; noise retains both the six-instruction preloaded-shift and seven-instruction
inline-shift-load schedules. Noise scaling/seeds and oscillator folding remain with the caller.

**[D]** `sdk/dsp/sample.inc` adds a five-instruction split-cursor step and bounded mono float32
linear reader. Six integer state words hold start/end/index/fraction/Q16.16 rate/done. It reads only
inside the half-open interval, clamps the interpolation neighbor and zeros the output after completion.
Resources, file loading, stereo, playback looping, boundary fades and anti-alias filtering are not supplied.
The caller must provide a valid mapped immutable source view; numerical index bounds alone do not prove
a SHARC memory mapping. This choice does not prescribe the stored PCM representation.

Parent verification: the two new test files and existing `tests/patchkit/test_dsp_kernels.py` total
**81 passing tests**. Numerical sampler tests cover exact integer-position oracle comparisons, reverse
fractional rates, large mapped sparse indices, zero/extreme rates, two-block continuation, source-read guards
and preserved registers. Module owner checks pass for BD (15 passed/7 skipped), PD (22/5), pluck (42/5),
with successful packaging/fit on both target devices. These are not new full firmware-device or hardware proofs.
Python lint and formatting pass; repository-wide changed-file checks retain the unrelated baseline path failures
documented in the plan. An independent source reviewer found no concrete issues in the two patches.

Instruction ceiling for the sample block routine including three caller argument loads: 1,380;
the two-call test harness measured 557–2,231 against its 2,761 ceiling. This is a sequential correctness
implementation and a concrete-core instruction count, not measured hardware cycles or an optimization claim.
Detailed contracts and usage: `sdk/dsp/README.md`. Raw firmware/core semantics were not changed.
