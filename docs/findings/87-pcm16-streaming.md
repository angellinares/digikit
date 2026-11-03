# Bounded PCM16 streaming for runtime sample loading

2026-10-29. **[D]** denotes inspected or measured evidence; **[O]** an unresolved
integration obligation. Work continues from the [SDK lifecycle extraction](86-sample-bank-sdk.md)
and is tracked in the [sampler plan](../plans/dsp-sdk-and-sampler.md).

## Source constraint and parser

**[D]** Both current device profiles reserve a 512 KiB CF arena and 64 KiB
staging buffer. The existing runtime source path allocates a complete native
file snapshot, then a second mono-float32 output. Ten seconds of 48 kHz mono
PCM16 is 960,000 payload bytes, so that path cannot stage the requested source
in this arena. The 4 KiB DT2 reads and chunked DN2 store snapshot still copy into
a caller-owned whole-file destination; chunked I/O alone does not remove this
capacity constraint.

**[D]** `modules/core/runtime/sdk/src/sample_stream.rs` now exports allocation-free
`NativePcm16Stream`. `push(input, output)` reports actual bytes consumed/written,
handles arbitrary input fragmentation and output backpressure, and writes only
complete four-byte words. Big-endian native PCM16 frames become little-endian
packed words for the existing DSP reader. An odd final frame has a zero high
halfword. Internal storage is one 64-byte header plus one pending packed word
and fixed-size counters, independent of sample length.

**[D]** The parser validates the canonical nonempty mono 48 kHz header, reserved
fields, zero trailer and exact end-of-file. Header length arithmetic is checked;
canonical total size must fit u32. The caller supplies a frame capacity, separate
from the existing decoder's 8,192-frame limit. A valid oversized header returns
E_TOO_BIG before output; malformed/truncated/extra input returns E_FORMAT. Errors
are sticky. `finish()` succeeds only after full validation and output emission.
Output before finish is staging data: a late error invalidates all prior output.
The caller must separately establish source consistency before publication.

**[D]** Existing `sample.rs`, runtime source acquisition, public float32 bank ABI
and actual firmware behavior are unchanged. The new parser is a building block,
not an integrated larger-bank loader. No runtime firmware object was rebuilt.

## Verification

**[D]** The shared SDK host suite passes 53 tests. New tests use the canonical
builder in `packages/dksample/src/native.rs` through a test-only module reference,
including arbitrary fragmentation, tiny/zero output, signed extrema, odd padding,
a 480,000-frame source through bounded output, malformed headers/trailers,
truncation/extra input, capacity and sticky errors. The builder only gains test-only
std imports; production behavior is unchanged. Existing float decoding tests pass.

**[D]** The owner check
`tests/patchkit/test_patchkit_dkrt_boot.py::test_the_sdk_record_code_passes_its_own_tests`
passes. Its stale exact test-total assertion was removed: the compiled test
process must succeed, without preventing new shared SDK tests. Host no_std library
compilation and scoped Rust formatting pass. Independent read-only review found
no qualifying issue. Logs: `out/runs/pcm-stream/compile.log`, `tests.log`,
`no-std.log` and `owner.log`. Existing unused-mut/dead-code warnings remain.

## Integration gates

**[O]** A closed, unready DSP staging bank can hold streamed output without a
whole-file CF buffer, but it needs an explicitly owned backing allocation and
source-consistency policy. The DN2 store's recorded digest and table fingerprints
are useful existing mechanisms; a streamed implementation must retain their
integrity checks. The current DT2 open handle is not an established immutable
lease. Native-format validation alone cannot prove that a file was not replaced
mid-read. On any uncertainty the bank must remain unpublished.

**[D]** The existing DSP-link uses spare frame capacity: 171 payload words per
DT2 frame and 12 per DN2 frame after its three-word overhead. Its current window
zero is the reserved L2 area, not a DDR allocator, and offsets are 17 bits. The
private DDR diagnostic instead substitutes full command frames and mutes output.
The intended production default is to preserve unrelated audio during loading;
no throughput measurement or hardware timing claim is made here.

**[O]** Historical DN2 1.11 evidence in [finding 31](31-dsp-link.md) warns that
reply rows used by link ACK/diagnostics also carry audio-return channels on
hardware. A current DN2 1.12 reply-area audit is in progress before extending the
live transport. The coupled emulator's cleared rows do not settle hardware
coexistence. No reply slot is newly claimed by this parser work.

**[D]** Routed changed checks pass Ruff and layercheck; only the ten baseline
stale references in five unchanged files fail. Scoped changed-path checks and
Rust formatting pass. Log: `out/runs/pcm-stream/changed.log`. No clean merge gate
or device integration result is claimed.


## Bounded DN2 store source

**[D]** `modules/core/runtime/sdk/src/store/stream.rs` now provides `FileStream`.
It retains a verified entry, header fingerprint and byte offset, without retaining
a table buffer or allocating the whole file. Initial lookup uses existing verified
store opening and redundant-copy fallback between matching fingerprints. Each
read checks the generation before and after bounded sector I/O. Returned lengths
exclude final-sector padding; no read exceeds the caller buffer or 128 sectors.
Errors are sticky and invalidate every previously staged byte.

**[D]** Exact EOF does not authorize publication. `finish` also checks the final
fingerprint and the caller's incremental SHA-256 against the retained entry's
digest. The caller must hash exactly the returned source bytes, before PCM packing,
and must wait for both source and native-parser completion before READY. Fingerprints
are commit detectors, not leases or protection against ABA; digest verification
checks the retained content. Once completed, the cursor returns its cached result
without reading the source again. This does not authorize rereading its old extent.

**[D]** Malformed-size testing exposed overflow in the shared sector-rounding
helper. Division plus remainder now replaces `bytes + 511`, preserving ordinary
results while allowing verified table bounds to reject an oversized extent.
The six added tests exercise multichunk reads and padding with the existing runtime
SHA implementation, replacement before/between/during reads, lookup changes,
read failures, late corruption, premature completion and sticky errors. The shared
SDK suite passes 59 tests; host no_std compilation and the named owner pytest pass.
Independent review found no issues. Production source loading and firmware objects
are unchanged; integration into the serialized sample job is still required.
