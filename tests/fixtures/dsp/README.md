# Private two-track binding proof

`sample-bindings.s` is an isolated assembled-consumer fixture, not an SDK ABI,
transport receiver, production pool, or hardware diagnostic. It includes the
existing SDK admission/epoch/count macros and actual PCM16 kernel; no numerical
or lifecycle primitive is reimplemented here.

Tracks 0 and 1 have independent controls (open, closed, count, fault, ready,
token), immutable source/frame descriptors and 128-frame packed PCM16 backing.
Other track values fail closed. Input routing uses generated `DK_IN_W_TRACK`.
Each private voice has six kernel cursor words, cached epoch +6, source +7 and
track +8. Cached ownership, epoch, admission and acquisition are checked under
one local interrupt-mask scope. Fresh note callbacks acquire before reading the
descriptor and release immediately afterward; render callbacks acquire before
using the cached pointer and release after the actual kernel's final read.
There is no note-lifetime pin or reliance on a note_off/voice-steal callback.

Private producer calls take track in R4, expected token in R8 and successor token
in R12 for publish. Result R0 is 0 complete, 2 pending, or -1 invalid. Close must
match track/token and closes admission before checking exact drain. Publish
requires closed=1, open=ready=0, zero count/fault, and a strictly increasing
positive signed 32-bit successor (initial token 0 may become 1). No retired token is
reopened and exhaustion cannot wrap. Faults are never cleared by the helpers.

One serialized producer fills exactly 64 words only after successful drain,
models completed payload delivery, then rechecks identity/drain and installs
source/frame/token before READY last. Its bounded fill is not a wait/spin loop;
there is no DMA or CF acknowledgment implementation. It cannot fill or publish
while a reader remains admitted. Real transport completion/ownership is a
separate prerequisite, absent from this diagnostic.

The Python test uses distinct A, B and producer stacks. It preserves A's paused
register/call/loop state while sharing DM, verifies A's live stack frame remains
unchanged across B's real kernel call, then checks preserved callee registers on
resume. This models scheduler interleaving, not physical interrupt entry/priority,
NMI exclusion, DMA completion, cache coherency, device backing ownership or timing.

Run only `uv run pytest -q tests/patchkit/test_dsp_sample_bindings.py`.

## Private CF close/ACK boundary

`sample-binding-close.s` includes the unchanged reader fixture and adds
`_bind_control(R4=command, R8=reply)` with normal-word DM pointers. The matching
`sample-binding-close.c` is compiled and executed by the existing CF call harness.
The test driver transfers complete immutable four-word numeric snapshots between
them. This is a logical protocol, not a physical wire byte order or transport ABI;
READY-last is not evidence of atomic four-word publication. Reply reads do not
race writes in this proof.

CLOSE is `{0x01014243, request_id, track, expected_epoch}`. Reply header is
`0x00014241 | (status << 24)`, followed by the identical ID/track/epoch;
status 1 is PENDING, 2 DRAINED, 3 REJECTED. IDs and epochs are positive signed
32-bit values; tracks are 0/1. One serialized producer retains its accepted tuple.
Lower IDs, conflicting identical IDs and higher IDs while pending are rejected
without changing accepted history or bank state. Exact retries recheck live
identity/drain, including after replacement. New IDs require the prior request
to have drained and a valid current binding identity; IDs never wrap.

CF retains its immutable request through PENDING. Only an exact matching DRAINED
snapshot authorizes reuse. Unrelated snapshots are ignored; matching malformed,
unknown-status or REJECTED replies quarantine the operation. Timeout also
quarantines it. Terminal DRAINED/FAILED outcomes ignore all late ACKs; beginning a
strictly newer request is possible only from initial idle or DRAINED. No memory
is freed and no sender-return or transport-terminal claim is made.

The interleaving proof pauses A inside the actual PCM kernel, retains its stack,
gets PENDING through both compiled CF and assembled DSP, renders B's exact old
PCM, resumes/releases A, then retries the same request and obtains DRAINED before
publishing A's replacement. Cached old A cannot read it; fresh A and unaffected B
produce independently derived exact PCM. A separate paused-B case checks that a
new pending request cannot inherit an older request's drained grant. Distinct
A/B/producer stacks and distinctive saved registers are checked.

Run `uv run python -m pytest tests/patchkit/test_dsp_sample_binding_close.py -q`.
The CF test uses `pk_cfcall.EXAMPLE` in this checkout by default; optionally set
`DIGI_BIND_CLOSE_CF_CALL` to an explicitly owned prebuilt runner. A missing binary
fails the prebuilt guard; the test never builds an emulator.
This does not prove physical IRQ dispatch, concurrent message snapshots, SPI/LP0,
DMA/cache coherence, production pool ownership or a public close/drain ABI.
