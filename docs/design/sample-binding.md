# Per-track sample binding

Design boundary, 2026-10-30. This follows the implemented
[file listing service](sdk-files.md) and the
[sampler plan](../plans/dsp-sdk-and-sampler.md). It does not export a new public
API or claim a production sample pool. The API 1.3 pinned-bank proof remains
unchanged while this replacement lifecycle is developed.

## Product behavior

- A sampler module owns independent bindings for source tracks 0..15, using the
  existing machine contract's latched track input.
- Browsing and backing out leave all bindings alone. Confirming a sample captures
  the target track, root, complete path and observed metadata, then queues selection.
- Selection stops that track's old sample notes. Other tracks keep their bindings
  and playback. A failed replacement leaves the selected track silent with an error;
  retaining its old audio through replacement is not required.
- The target is full stereo with start/end/reverse and long samples constrained
  by actual shared backing capacity. Sixteen tracks share deduplicated project
  backing; there is no arbitrary ten-second or 16 MiB cap. Directory rows
  establish neither sample validity nor a lease.

## Ownership and ordering

Separate three identities: the module-owned client, the selected track, and a
nonreused request/generation token. A completion must match all three before it
can update visible state. Repeated selection may replace pending work, but cannot
release buffers or backing still used by an earlier active operation. Module
teardown has the same requirement. Counter exhaustion fails rather than wrapping.

One producer serializes each binding's transition. The initial transport may
serialize loading across tracks; that must not close another track's reader
admission. Keep queued path/identity storage separate from an active source stream,
DMA source buffer and DSP destination metadata.

The transition order is:

1. **Stopping:** send a matching generation-close operation to the DSP. It closes
   admission for the old generation and prevents new notes from acquiring it.
   Do not overwrite its backing or claim retirement merely because CF queued the command.
2. **Draining:** wait for a DSP acknowledgement proving admission is closed,
   all readers have released, bookkeeping is sound, and any transfer touching
   that backing is terminal. Never spin inside an interrupt while waiting for
   interrupted audio code to release its reader.
3. **Loading:** revalidate/open the selected source and decode/transfer bounded
   chunks into owned backing. A cancelled or superseded transfer retains its
   buffers until its exact terminal completion; then its result is discarded.
4. **Publishing:** after complete source validation and payload completion, install
   the immutable descriptor for a fresh nonzero token and publish READY last.
   CF reports ready only after a matching DSP acknowledgement.
5. **Ready or error:** notes may use only the published generation. Errors remain
   explicit; no partial payload or stale completion may become a playable binding.

These are semantic phases, not finalized public enum values or wire messages.
Source validation can be scheduled earlier if it does not weaken the decision
that selection closes the old track and does not expose unvalidated audio.

## DSP reader boundary

Use the existing admission, epoch, count, close and drain operations in
`sdk/dsp/sample_bank.inc`. They are useful primitives, not a complete allocator
or cross-processor handshake. A reader must acquire before its first descriptor
or PCM access and release after its last possible backing access. Check and
count updates use short local interrupt-mask scopes; do not mask interrupts for
the whole audio render or wait for DMA under that mask.

The current adapter has `note_on` and `render`, with no module `note_off` or
voice-steal callback. Therefore do not hold a reader count for a note's entire
lifetime: an idle or stolen voice might never return to release it. Instead,
cover each callback's actual backing accesses. Fresh notes latch the ready epoch;
later renders check their cached track identity and epoch before dereferencing
their cached pointer. Per-track tokens can be equal, so token equality alone
does not establish which binding owns that pointer. Admission, cached ownership,
epoch validation and count acquisition share one masked scope; checking
first and acquiring in a later scope would leave a retirement race.
Admission failure or stale epoch returns `DK_DONE` without touching PCM.

Closing admission and observing an exact zero reader count can retire a generation
without forcing every inactive voice to render. If such a voice runs later, its
old epoch must still be rejected. Never reopen a retired token. Corrupt counts or
faults must fail closed and prevent reuse, as required by the existing macros.

This needs an integration proof around every reader, not just numerical macro
tests. The current sample-proof directly dereferences its latched pointer and
does not yet implement this protocol. DMA is a separate reader/writer participant;
CPU interrupt masking alone does not establish DMA completion or cache coherency.

## Remaining platform evidence

The private DN2 0.2 transport already proves device-side CF completion ACKs for
the bounded payload while stock audio continues in the emulator. That is not a
proof that an old audio generation has drained, nor ownership of the destination.
See [reply ownership](../findings/88-dn2-reply-ownership.md) and
[the bulk seam](../findings/92-dn2-native-bulk-seam.md).

The production pool still needs a reservation and lifetime contract on each
device. DT2 project sample memory and DN2 configured DDR geometry cannot be
treated as interchangeable allocators. Output-pointer derivation is resolved in
[the DN2 DDR audit](../findings/106-dn2-ddr-configuration.md); ownership, lifetime
and coexistence remain unresolved. Continue from those obligations rather than
repeating the bounded fixed-object rings or assuming a high address is free.
Storage/source validation, checked destination extents, cache behavior,
transport timeouts and recovery must be established before exposing usable bindings.

## Next proof

First specify a private DSP binding descriptor/control layout and exercise two
tracks with the existing emulator fixture and guarded backing. Prove that closing
one track rejects its old cached pointer, drains an interrupted reader safely,
and leaves the other track producing its prior audio. Cover a note arriving during
close/load, supersession, stale acknowledgements and failed publication. This
bounded proof is independent of claiming a larger production pool.

Reuse the prior single-bank paused-reader method in
`tests/patchkit/test_dsp_ddr_proof.py` and the SDK's two-bank macro tests in
`tests/patchkit/test_dsp_sample_bank.py`. The new proof must add track routing and
unaffected-track audio, not repeat those tests under new names. Keep this as an
isolated assembled-consumer test; preserving a paused runner's register/loop state
while sharing memory models an interleaving, not physical interrupt dispatch.

Only after the descriptor/retirement boundary and platform backing are supported
should runtime bindings and the on-device picker publish a ready sample. The
picker can reuse the public listing service and SDK UI components, retaining the
captured target track throughout navigation and asynchronous completion.

Independent source/design review found no unsafe ordering or contradiction with
the current adapter and macro contracts. The private two-track assembled-consumer
proof now passes 15 focused tests and independent review; see
[binding lifetime evidence](../findings/94-sample-binding-lifetime.md).
Physical interrupt dispatch, runtime transport and all platform ownership
obligations above remain open.
