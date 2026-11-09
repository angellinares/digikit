# Sample generation reopening: decisions and proof boundary

2026-10-29. Analysis targets: DT2 OS 1.17 and DN2 OS 1.12. **[D]** records a
source read or measured check; **[O]** marks a proposal or unresolved obligation.
No new [V], public sample ABI, hardware or performance claim. This follows the
[terminal retirement proof](83-sample-retirement-design.md); its existing evidence
remains there. Current work is tracked in the
[sampler plan](../plans/dsp-sdk-and-sampler.md).

## Prior independent cached-voice guard checkpoint

**[D]** The diagnostic consumer now reserves 28 bytes of state and clear size:
the first six words retain the SDK cursor contract, and private word 6 holds the
admitted bank generation. A note initializes that word to zero, then captures the
current nonzero token only with READY=1 inside the existing masked admission
section. Render compares the cached token with the current token in that same
section before incrementing the admitted count. It cannot adopt a newer token.
Denied callbacks invalidate the word and return silent DONE. Counter corruption
still fails closed; the numerical reader still runs with incoming MODE1 restored.

**[D]** Forty-two focused assembled diagnostic/CF tests pass, including six new
cases covering a changed global token, invalidated cached renders, fresh-note
capture, denied notes and armed-like cursor contents. The rebuilt DSP object
re-decodes 832 instructions and passes its freshness check. This exercises actual
consumer assembly with modeled control-word changes; the wire protocol remains
unchanged. It does not prove an actual bank reopening or a queued note's age.

**[D]** Direct Python lint/format, scoped TypeScript lint/format, regression types,
path and diff checks pass. Host checks pass 193 with 16 skipped
(`out/runs/ddr-proof/generation-host-checks.log`). Routed changed checks retain ten
known path-reference failures and the pnpm launcher failure
(`out/runs/ddr-proof/generation-changed-checks.log`); no clean integration gate is
claimed.

**[D]** The parent reports all five existing coupled cases pass against the changed
consumer/state size: full transport/ten-second playback, three fresh-boot failure
cases and terminal retirement during playback. Log:
`out/runs/ddr-proof/voice-generation-suite.log`; its 119.59-second wall time is not
a performance measurement. Normal artifact:
`out/runs/ddr-proof/55f9f2f3-ff27-47d5-aa3b-1d80f4054be6`; terminal artifact:
`out/runs/ddr-proof/d1974ff3-48fa-46f1-8bec-6feb6b6f9314`. This completes the
independent cached-voice guard check, not a generation-reopening wire test.

## Selected queued-note policy

**[D]** The user selected the recommended policy: stop currently playing notes on
selection; a queued note that starts after the new bank becomes READY plays that
new bank. This preserves future sequence events. Strict selection-time event-age
rejection and timeline clearing are not required for this policy. Cached old
voices still require retirement and generation checks before backing reuse.

**[D]** The private reopening protocol below is implemented and locally checked.
**[D]** All six coupled cases pass; the tightened C-audio assertions also pass in a focused rerun.

## Implemented private protocol and coupled proof

**[D]** The next bank token must be the exact boot-scoped successor `current + 1`, accepted only after the
prior generation is exactly RETIRED, admitted count is zero and fault is zero.
Tokens must never reset to zero, be reused or wrap; exhaustion fails closed.

| Operation | Sequence / status | Behavior |
| --- | --- | --- |
| INIT / DATA | Existing transfer sequences / 0 | Load only the accepted successor generation while admission remains closed. |
| COMMIT | 376 / 1 | Validate the complete bank and seal it with admission closed. Status 1 must never open admission automatically. |
| OPEN, op 5 | 378 / 7 | Open the exact sealed generation; later starts resolve the new bank under the selected policy. |
| RETIRE, op 4 | 377 / 5, or 6 while pending | Close and drain the exact generation; a complete sealed bank may retire without ever opening. |

Control sequences are exact operation identities, not a generic increment rule:
COMMIT → OPEN → RETIRE uses 376 → 378 → 377. CF must match the exact operation,
token, sequence and success status. A duplicate OPEN received after RETIRE must
never reopen that bank. Generic status-1 auto-opening has been removed.

**[D]** To skip B after starting its load, the private writer finishes and seals B,
retires B, then begins C. There is no cancellation. `DdrWriter` serializes whole
load transactions and retains failure on its owner chain even when callers catch
the result; no queued or later C continues after uncertain B completion.

**[D]** CF validates and caches the complete staging request on START, then reuses
that classification during immutable BUSY retries. Exact successful DSP ACKs alone
grant held-note substitution: RETIRE grants successor INIT; INIT grants loading
DATA/COMMIT; COMMIT grants OPEN. Sealed COMMIT and opened OPEN have narrow duplicate
control grants. INIT/RETIRE clear the old OPEN grant; no OPEN grant permits DATA.
All bypasses require the valid normal-frame context. Transfers mute shared output
in this diagnostic; this is not a production live-audio ingress or throughput result.

Malformed staging headers, ranges and checksums now stop CF before DSP submission
with error 101. DSP independently retains its checks. Rejected owned ordering can
retain the prior accepted ACK sequence, so CF may time out rather than consume a
matching DSP error ACK; both outcomes are sticky and prohibit further mutation.
Stale tokens cannot change the bank or ACK identity. The local CF hook disables
compiler jump tables to keep table data out of instruction-checked text sections.

**[D]** Sixty-eight focused assembled diagnostic/CF tests pass, including successor
criteria and exhaustion, early/duplicate controls, stale INIT/DATA/COMMIT/OPEN/RETIRE,
exact ACK status and grant handling, malformed staging, and preserved-reader
retire/successor interleavings. Two host writer tests pass for whole-load
serialization, sealed-B skip and permanent queue stop after captured failure.
Objects rebuild/re-decode 370 CF and 912 DSP instructions and pass freshness checks.
Direct Python lint/format and scoped TypeScript lint/format/types pass.

**[D]** All six coupled cases pass in `out/runs/ddr-proof/generation-reopen-suite.log`.
The 202.04-second suite wall time is not a performance measurement. The existing
cases use explicit OPEN before playback; malformed range/checksum staging is
rejected by CF with error 101, not a coupled DSP-poison observation.

The A → B sealed skip → C artifact is
`out/runs/ddr-proof/5103437f-373f-4f40-b9f9-c2ef10a73698`. A retires while active;
B is fully loaded, sealed and retired without opening; C opens and a fresh note
captures token 2166572393. B's full guard bands are checked before C INIT reseeds
them. C's complete source/readback SHA-256 is
`7c942f58087983416e16862cc3141a55fa43e27bbd93ffe44d25fb80fc49b448`;
each guard hash remains `539a03c496fbb7fca763db383a58ecb3373356a98a31b79fd5421277a9652cd5`.
Measured powers at 200/300/400 Hz are approximately
`[0.0001559267, 0.0005086457, 25467.34568]`.

**[D]** The parent's independent `generation-c-audio-audit.json` records all 72,544
captured frames finite, peak 0.02068265 and mono RMS 0.0094734175 over frames
14400..38400. This establishes absolute audibility in addition to relative content.
The test now records full-capture finiteness, window RMS and frequency powers
before audio verdicts, and requires finite PCM and RMS above 0.001 as well as
400 Hz dominance. **[D]** The focused rerun passes in
`out/runs/ddr-proof/generation-reopen-audio-check.log`; artifact
`out/runs/ddr-proof/9a8eff19-98bf-4d2d-b643-8461f07fd6a2` records 72,448 finite
frames and RMS 0.0094734175. The other five passed cases required no repeat for
this assertion-only change.

**[D]** Final stale-B RETIRE transmits 900 times before CF watchdog error 100;
C's token, READY/admission, full bank and guards remain unchanged, and normal
frames resume. Host checks pass 195 with 16 skipped
(`out/runs/ddr-proof/reopen-host-checks.log`). Changed routing retains the ten known
reference failures and pnpm launcher failure
(`out/runs/ddr-proof/reopen-changed-checks.log`); no clean integration gate is claimed.

**[O]** The next recommended work is to port the owned-bank lifecycle proof to DT2,
then extract the reusable SDK/runtime contract for both devices. No public API or
new hardware pool ownership is approved by this diagnostic. Hardware cache/coexistence
and production live-audio ingress remain separate. The selected queued-note policy
is settled; another DN2 reopening proof is not the next design task.

## DN2 queue discovery relevant to that decision

**[D]** One-pass current DN2 1.12 decompiler/scout reads report the following. These
are discovery observations, not byte-audited [V] facts or approved interception
points. Reproduce reads through the existing `tools.coldfire.fw` decomp/dossier
workflow documented in [finding 83](83-sample-retirement-design.md).

- `ADM_TRIG_HW` value 17 maps to frame `0x80005e60 + 0x22 = 0x80005e82`;
  `0x80005e84` is not that field. Builder `0x40025e36` writes the high half of
  `local_b0`. Its generic event path resolves note/velocity through `0x4002a6a2`
  and ORs the allocated mask when a payload field shown as `local_28[0xe]` in
  the decompiler passes a signed flag test. The exact record layout, event type
  and all producers remain unproved.
- Timeline insertion `0x40138e3c` handles heterogeneous timestamped nodes;
  the payload chain is at offset `+0x68`. No spare or unused payload bytes have
  been established for a generation field.
- `0x400d7d76` pops through `0x40138cb6`, sets type 7 and inserts an event.
  `0x400d7e88` copies sixteen `0x6c`-byte records from `0x446494c4` and inserts
  them with inherited type. `0x400255b4` copies a type-2 control record.

**[O]** Clearing this heterogeneous timeline is unsafe as a general note fence.
A quiet frame does not establish an event-age fence either. Strict discard was
not selected; a future change to that policy would require a centralized generation-
capture candidate and coverage of all producers before implementation. Existing pending-frame,
DMA and callback assumptions in finding 83 still apply.
