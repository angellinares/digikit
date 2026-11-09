# Sample loading and lifetime: next proof boundary

2026-10-29, following SDK checkpoint `4cd29a3c`. DT2 1.17 and DN2 1.12. **[D]** source/database
observations; **[O]** design or unresolved behavior. No new firmware execution or hardware timing.
Nothing here is [V]. See [finding 104](104-dsp-sdk-sampler-discovery.md) and
[the implementation plan](../plans/dsp-sdk-and-sampler.md).

## DT2 source acquisition

**[D]** DT2 Transfer uses stock sample RPC writes to +Drive; DN2 uses the digikit store. The host
RPC client does not implement sample file reads (`packages/dk-session/src/rpcfiles.ts`). That does
not prevent an on-device reader, and adding a host read command is not necessary for this proof.

Current-image matches were established with `fw port`, not by querying old addresses in the new image:

| API candidate | DT2 1.16 | DT2 1.17 | Matching evidence |
| --- | --- | --- | --- |
| path open | `0x40158626` | `0x40158902` | strict, unique function hash |
| read | `0x4015832e` | `0x4015860a` | strict, unique function hash |
| close | `0x401585e4` | `0x401588c0` | strict, unique function hash |
| size | `0x4015ba22` | `0x4015bcfe` | strict, unique function hash |

**[C]** An initial scout queried historical addresses directly on 1.17 and got containing functions.
That is not porting evidence. Those nearby entries were discarded. Missing decompilation/disassembly
was initially an absent worktree link to existing `out/ghidra/`, not absent original analysis inputs;
the parent linked that input directory read-only and inspected current dossiers.

**[D]** Current `0x40158902` decompilation checks mount state, non-null arguments and a path length
below 256 bytes. It searches the mode string for character `0x77` (`w`); the existing-entry read
branch sets open/position/mode fields and returns without taking the creation/write branch.
Caller setup in `File::ctor_dtor` passes handle, a mode string and path on the stack.
Current `0x4015860a` checks mount/arguments/open flag, takes the filesystem mutex, calls its internal
reader with file id and current position, advances position by the returned count, unlocks and returns.
The old, hash-matched API description in [finding 23](23-digikit-runtime-spikes.md#s3-reading-files-from-our-code-v-unless-marked)
gives open(path, mode, handle), read(buffer, length, handle), close(handle), size(id), and a 16-byte handle.
Size takes the internal entry id, not a pointer to the handle. A zero-initialized handle should only be closed
after a successful open. Current open-mark helper `0x4015bf40` uses a per-id bitmap; this is not evidence
of a reference-counted immutable-file lease across multiple readers.
This is sufficient to specify a narrow adapter proof, not to claim it has executed successfully on 1.17.

The wrapper must open only in read mode, close on every terminal source path, check actual byte counts,
and discard the handle on errors. It must not wrap the API in an extra filesystem lock: the routines
lock internally and the historical owner-aware mutex is not a recursively counted lock. No generated
bindings for this path API currently exist. New declarations and a bounded runtime proof are required.

**[D]** `modules/core/runtime/src/rs/src/lib.rs:941` places `dkrt_tick` in the UI task event loop,
and that function calls `dsp::step`. It offers a task-context job pump. **[O]** Maximum blocking time
of file/card reads and their effect on UI/audio on the target are still unmeasured; chunk size must be
bounded and measured, not described as nonblocking merely because the outer job is asynchronous.

## Source snapshots on DN2

**[D]** `store::commit` writes data/table in free space and switches headers. A later transaction may
reuse the old extent. `File { first, size, sha }` is metadata, not an asynchronous lease. Source:
`modules/core/runtime/sdk/src/store.rs:759`, runtime `upload.rs:708`, `store.rs:92`.

`store::stable` fingerprints headers around a closure and retries changes, but after `STABLE_TRIES`
it returns the last result; it also returns if the first fingerprint is unavailable. This is not a
fail-closed stable-snapshot guarantee (`modules/core/runtime/sdk/src/store.rs:399`). Public `store_read`
also uses shared staging. A new asynchronous sample job cannot retain a staging pointer or a saved
disk extent while unrelated uploads continue.
`STABLE_TRIES=4` means four retries, up to five attempts, not four total attempts.

**[O]** For the bounded proof, obtain the complete source into a private ColdFire buffer before starting
DSP delivery. Use private lookup/read scratch, source identity/hash and before/after store generation
checks; changed or unavailable metadata must fail or retry within a bound, never publish the last
unverified attempt. Hash the actual private bytes that will be decoded. Later file replacement cannot
change that snapshot. For a larger chunked loader, an explicit lease or a complete validated snapshot
transaction across chunks is required. The proposed proof does not claim to solve that larger case.

For DT2, use a valid read handle and complete private copy, but separately prove how overwrite/delete
interacts with that handle. A mutex taken per read is not a file-lifetime lease across reads. The first
device proof should run without simultaneous Transfer writes; concurrent replacement must either be
prohibited by an enforceable rule or have tested detection/handle semantics before it is supported.

## DSP ownership and transfer state

**[D]** `alloc_range` tracks one extent per loaded module (`dsp.rs:470`); the extent includes image,
BSS and voice/effect state. `Pending`, waiting queue and rollback are module-load-specific. There is
no independent sample allocation owner or generic runtime DSP-job API.

**[O]** A first sample bank can be a declared module-owned DSP BSS region, counted in the existing
module extent, rather than a second allocator. The SDK/packer must declare and validate the bank
and descriptor offsets against that module's data/BSS region; copying arbitrary caller DSP addresses
is not the interface. The runtime uses those validated offsets and serializes its job with existing
module transfers. This still needs a versioned packaging/descriptor addition; BSS alone is not a new API.

**[D]** `dl_send` retains the caller buffer and `write_chunk` dereferences it on later ticks
(`modules/core/adapters/dsp-link/src/cf/dsp-link.c:110`, `:200`). Normal runtime completion frees
source only after DONE. The runtime guard error path instead calls `fail`, which frees `send_buf`
without first proving the link terminal (`modules/core/runtime/src/rs/src/dsp.rs:1110`, `:1136`).
**[O, conditional risk]** If UI passes progress while DSP frame ticks stall, the guard could free a
buffer that the link will read when frames resume. This schedule has not been reproduced. New sample
jobs must not inherit that assumption; test stalled transport and resumed ticks with fake-link state.

The job state should distinguish user-visible failure/cancellation from transfer quiescence. Keep a
source buffer pinned while BUSY/START, even after the job reports failure; release it only when the
transport can no longer read it. Do not dispatch another transfer from an ambiguous owner state.
If recovery cannot establish quiescence, leave the buffer quarantined until restart and report that
bounded resource loss. A shared cleanup fix belongs in the transport implementation scope if required.

**[D]** DSP-link window 0 already writes inside `_dl_area`; windows 1/2 are voice/FX tables. Sample
bytes and a descriptor within an owned bank can therefore use window 0, without a new unbounded
write window. Current code flushes instruction caches after LAST and before acknowledgement
(`dsp-link.inc:214`); data visibility/coherence for the new use still needs proof. Do not call the
existing instruction-cache sequence a proven data-cache ownership protocol.

**[O]** Publication sequence: initial descriptor READY=0; transfer decoded payload; wait for acknowledgement;
write immutable descriptor fields; acknowledge them; write the one-word READY marker last; acknowledge it;
report READY to control side. Never mutate or reuse a published bank during this proof. Voices read only
after READY and latch the immutable view at note-on. Faults, retriggers and voice stealing cannot outlive
the bank because the bank is pinned until restart. Resource reclamation is deliberately deferred.
The initial descriptor belongs in initialized DSP data sent with the module image, not uninitialized
BSS. Admit sample jobs only after the owner module is fully loaded; a sample failure must not use
boot-module rollback to free the bank. Even if the final READY acknowledgement is lost, retain the
destination until restart because DSP publication may already have occurred.

## Proposed bounded proof and tests

This is not implementation approval. Suggested proof limits: one module bank, mono native file, at most
8,192 frames, 48 kHz. Decode PCM16 into the existing float32 reader's view: **32 KiB DSP payload**,
about **171 ms** of audio. A native file of that length is **16,464 bytes** including header/trailer.
Bank metadata, code and voice states are additional. Fit is checked against all selected modules;
do not promise universal coexistence. The tiny limit is for a transport/lifetime proof, not a product
sample-length limit or final PCM representation policy.

Use the current Transfer destinations and select a known uploaded file by path for the proof. DT2's
source is its read-only filesystem adapter; DN2's is the store. Reject stereo/oversized/malformed input
explicitly; no silent downmix or truncated-success result. Keep selection UI, precise trimming and stereo
outside this milestone. The render routine can use fixed test bounds/direction/rate before product knobs.

Proposed validation:

- Host tests for bank bounds/accounting and version/owner validation.
- Mock card commits at lookup/read boundaries; fail closed on unstable identities or short/corrupt input.
  Existing seam: `modules/core/runtime/sdk/src/store/tests.rs:466`.
- Fake-link tests for busy queue, partial transfer, timeout/cancel, resumed ticks, and buffer retention;
  READY must remain false until every preceding transfer completes.
- Existing numerical reader tests plus emulator scenarios using actual transferred files on DT2 1.17 and
  DN2 1.12; assert start/end/reverse PCM, silence before READY, and memory bounds after failure/retrigger.
- Bound all runs and report actual loading latency, memory and DSP instruction cost. No hardware proof
  without Em's separate authorization. No new transport bandwidth or streaming claim.

Reproduction of the mapping and current body inspection:

```sh
uv run python -m tools.coldfire.fw batch - <<'Q'
port 0x40158626 --from dt2-1.16 --to dt2-1.17
port 0x4015832e --from dt2-1.16 --to dt2-1.17
port 0x401585e4 --from dt2-1.16 --to dt2-1.17
port 0x4015ba22 --from dt2-1.16 --to dt2-1.17
dossier 0x40158902 -i dt2-1.17 --decomp 70 --callers 3 --max 0
dossier 0x4015860a -i dt2-1.17 --decomp 24 --callers 2 --max 0
Q
```
