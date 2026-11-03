# DN2 reply-channel ownership before runtime sample integration

2026-10-29, DN2 OS 1.12. **[D]** inspected current-image evidence; **[O]** unresolved.
This follows [streaming preparation](87-pcm16-streaming.md) and the
[sampler plan](../plans/dsp-sdk-and-sampler.md). An independent evidence agent
checked the decision-critical paths against original bytes; no device was contacted.

## Occupied row channel

**[D]** Current section 7 is 836,956 bytes, SHA-256
`336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`,
matching the current port and database. The common-tail anchor at `0x1c9e5f`
matches original bytes. The tail reloads reply I4 from `DM(I6-2)` at `0x1c9e4f`
and writes stock word 0 at `0x1c9e62`. Reply selection at `0x1ca03a..45` is
`0x2c49d0 + (DM(0x2c0450) << 12)`.

**[D]** CF reply copy is `0x800053a4`, with rows starting at `+0x1c`.
The existing ACK at `+0x60..67` occupies row 0 bytes `+0x44..4b`; diagnostics
also use these offsets in later rows. In the mode-6 branch of ISR `0x40025e36`,
the row pointer `0x800053c0` is passed to `0x40003118`. That routine computes
row addresses with stride `0x54` and copies every byte of each row into outgoing
records at `+0x38`. This establishes an actual current-image consumer path for
the ACK/diagnostic bytes. It does not independently establish historical channel
numbers or measured audible consequences.

**[D]** The stock DSP row writer receives reply R12 into I13 at `0x1c2479`,
restores that pointer at `0x1c26a5`, then forms reply `+0x1c` at `0x1c26af`.
The packing loop at `0x1c26b2..ec` writes the rows. Its writes positively exclude
the header. Extending live sample transport must not assume the row area is free.

## Header candidate and counterevidence

**[D]** Original loader block 40 initializes both reply headers to zero at aliases
`0x282c49d0` and `0x282c59d0`; this establishes startup contents only.
A known halfword writer at `0x1c3117` targets header `+0x16`.
Descriptors built at `0x1ca46a` receive length `0xabc` from `0x1ca812`, derive
count `0x55e`, and install both reply addresses at `0x1ca513..552`. CF driver
`0x400cf922` copies `0xabc` bytes from DMA staging `0x80001a20` to the reply copy
before rearming DMA; header `+0x08` is included.

**[D]** Raw CF code outside known database function boundaries at `0x400cfbe0`
reads halfwords `0x80001a28/2a`, combines them into a 32-bit result, and returns.
This is a stock accessor for staging reply `+0x08`. No direct reference or literal
function pointer was found, which establishes neither live use nor dead code.
The neighboring accessor `0x400cfbf4` reads `+0x16` and has an actual caller at
`0x40070412`. Whole-main reference scanning covered 96.61% of bytes; computed
addresses/calls and asynchronous DMA are outside that proof.

**[O]** No exclusive header `+0x08` ownership or safe second ACK word is established.
Remaining edges are writers between construction and DMA consumption and the
reachability of `0x400cfbe0`. Empty references and repeated zero snapshots cannot
prove exclusivity. A compact one-word acknowledgement is only a conditional
candidate, not an approved allocation.

## Probe and implementation boundary

**[D]** Existing `dklog peek` cannot read the relevant area: current DN2 PEEK
ranges in `devices/digitone-ii.toml` exclude `0x80000000` RAM, and
`modules/core/runtime/src/rs/src/upload.rs` enforces them. A possible bounded
observation-only probe would copy staging/reply headers into module-owned,
PEEK-readable RAM, without writing the reply. It has not been implemented or
run; uploading or running probe firmware on hardware requires a separate concrete
proposal and user authorization. Snapshots can detect contrary evidence but
cannot exclude asynchronous writers.

**[O]** Production integration must resolve this channel ownership rather than
inherit the diagnostic ACK layout. The existing spare-frame channel has 12 payload
words on DN2, at most 48 bytes per 32-sample block. At 48 kHz that is a theoretical
72,000 bytes/second ceiling, so 960,000 bytes require at least 13.33 seconds before
acknowledgement, framing or host overhead. This is a capacity bound, not a benchmark.
The full-frame diagnostic is faster in principle but substitutes audio commands
and mutes output in its emulator proof. The user confirmed per-track changes with unrelated audio preserved; no hardware performance claim follows from either route.


**[D]** Follow-up traces the row copies through `0x40003118` to its transfer
submission at `0x40003352..58`: a `0x3f4` descriptor on endpoint index 7,
identified by the existing USB findings as Overbridge EP3 IN. Thus these bytes
reach an outgoing USB stream. Historical DN2 1.11 live-row measurements in
[finding 31](31-dsp-link.md) identify the overlapping bytes as parts of
channels 22..25 and show playback-dependent content. That historical measurement
supports the risk; exact DN2 1.12 host interpretation and audible consequences
remain unmeasured. Copying the bytes does not itself prove how a host uses them,
but the existing link comment calling the region unused is insufficient for live
sample-loading ownership.

## Completion-field follow-up after LP0 proof

**[D]** The native LP0 payload and stock-audio proofs in
[finding 92](92-dn2-native-bulk-seam.md) observe completion through emulator
memory reads. They do not provide a device-side completion protocol.

**[D]** Raw row-loop counts at `0x1c26b2/26c4` are 14 stereo channel pairs and
32 samples, respectively. At six bytes per pair and `0x54` stride, the rows
cover reply `[+0x1c,+0xa9c)`. The remaining 32 bytes before transfer length
`0xabc` are not established spare space: stock accessor `0x400cfbfc` reads
signed halfwords from staging `+0xa9c + 2 * argument`, maps negative values
to zero, and otherwise returns the value shifted right seven bits. Arguments
0..15 cover the entire trailer. No argument bound is enforced by this accessor.

**[D]** Header accessor `0x400cfbe0` combines two unsigned halfwords at `+0x08`
as `(first << 16) | second`. Whole-main reference scanning at 96.61% coverage
found no external reference to either accessor; that still excludes neither
computed calls nor asynchronous producers. Trailer producer coverage remains
incomplete. Finding 31's categorical claim that no stock code writes header word
2 exceeds this bounded audit's evidence.

**[O]** Neither header nor trailer can yet be claimed for completion. The next
bounded feasibility question is an explicitly owned extension beyond the stock
reply, preserving all original bytes and consumers. It must account for both
DMA counts, actual buffer extents, clocking and CF copy lengths; spacing between
buffers alone does not authorize enlargement. These observations alone do not
establish a usable completion channel.

## Explicit extension feasibility

**[D]** A bounded original-byte audit finds a numerically plausible 16-byte
extension (`0xabc → 0xacc`), with coordinated boot-time changes on both CPUs:

| Control | Current DN2 1.12 path |
| --- | --- |
| CF wire size | `0x400cf08c → 0x40027d3a` returns `0xabc`; `0x400cf7ec` stores it at `0x42441958`. |
| CF DMA | `0x400cf922` caps bytes at `0xaf0` and programs TX29 and RX28 with count/2. `0xacc` would require `0x566` transfers. |
| Stock reply copy | Caller `0x40025e90` independently supplies `0xabc`; copy at `0x400cf956` uses staging `0x80001a20` and destination `0x800053a4`. |
| DSP descriptors | `0x1ca812` supplies `R4=0xabc` to `0x1ca46a`; length/2 at `0x1ca502` feeds all four ping-pong RX/TX descriptors, with two-byte stride. |

**[D]** Preserve the stock copy length: `0x800053a4 + 0xabc = 0x80005e60`,
the beginning of the stock TX payload. Extending that copy would corrupt TX
data. Any completion consumption must be separate from the stock copy.

CF staging begins at `0x80001a20`; TX PUSHR storage begins exactly `0xaf0`
bytes later at `0x80002510`. Its maximum `2 * 0xaf0` span ends at `0x80003af0`,
where stock initialization writes another object (`0x40025b20`). At proposed
size `0xacc`, both numerical spans end before those neighbors. On DSP, the four
main roots are `0x2c29d0/39d0/49d0/59d0`. Auxiliary DMA setup at
`0x1ca5b3..5da` installs buffers at each main root plus `0xaf0`, leaving 36
bytes between a proposed `0xacc` main extent and the next auxiliary buffer.

**[O]** These are positive size and neighboring-object facts, not proof of
exclusive ownership of every intervening byte. Boot counts must agree before
DMA starts; resizing armed descriptors is excluded. Private-buffer redirection
seams exist at CF RX DADDR and staging memcpy source, and DSP ADDRSTART/selector
initializers, if ownership requires separate allocations. These findings motivated
the isolated boot-time diagnostic below; they establish no production wire contract.

## Private 0.2 completion proof

**[D]** `core/dsp-lp0-proof` 0.2 implements the isolated extension. Only the CF
boot getter call is retargeted; the original getter and stock copy remain `0xabc`.
The DSP boot literal is `0xacc`. The memcpy-call wrapper preserves the original
arguments and return value, then reads the extension from staging before rearm.
The eight numeric halfwords are magic `0x4c50`, version 1, status, error, page
count low/high, and byte count low/high. DSP snapshots under a local interrupt
mask with scalar stores and restores context. CF publishes through a seqlock,
suppressing identical snapshots so observation can stabilize between packets.

CF retains each START, waits for exact pre-send progress, sends once, then waits
for exact post-send progress. Intermediate completion requires status 2; final
completion requires status 3, pages 235 and bytes 960,000. Missing/old replies
remain pending; malformed valid-tag replies and errors fail closed. DONE now
means DSP acknowledgement rather than sender return. The stock send itself is
still unbounded, and this private single-boot proof adds no public epoch protocol.

**[D]** The original cold-idle already has four queued `0x55e` descriptors,
although its live DMA XCNT registers are still zero. Pre-init `cold.state` has
PC `0x1c12e2`, an empty memory overlay and uninitialized SPI2 DMA. Its SHA-256 is
`6c071edf7eb12f359197fbc453ce96f916dd8bffff660102a83a5cd9ebe585e7`.
The parent built only `dsp_cold` against the cached core, applied the patch before
execution, and reached idle PC `0xb88a5d` at 563,750,000 instructions within an
800-million-instruction/120-second bound. All four descriptor XCNT fields are
known `0x566` before CF exchange. There were no live descriptor edits or core
regeneration. Exact commands, hashes and counts are retained in
`out/runs/lp0-reply-fixture/provenance.json`; new cold-idle SHA-256:
`ad6429f7a19dee6e1f14696fcdb074cb54adbbf39cc543271d0ff132ed00d248`.

**[C]** The first native attempt failed with a legitimate frame-length mismatch:
the daemon canonicalizes the DSP image path and selects its sibling cold-idle.
A private image symlink therefore selected the old stock directory. A physical,
byte-identical private image copy fixes selection; native preflight now checks
the canonical parent. Original inputs remain untouched. This was a fixture error,
not evidence of an emulator or firmware defect.

**[D]** Corrected first-page proof passes in
`out/runs/lp0-proof/8c974a38-73d6-42d5-967c-bf678ecf566d`.
Full-bank held-stock-audio proof passes in
`out/runs/lp0-proof/5e0b6f2a-84ac-4814-bc6e-57794f493b38`:
235 coherent CF ACKs, 960,000 exact known bytes (SHA-256
`0eb5375f0383e6dc3422abcf93926ebdff4632af0c1c6bc06598ecbbb00d4a72`),
unchanged known guards, final ACK sequence 476/status 3/pages 235/bytes 960,000,
and zero terminal DMA configuration/count/status after subsequent frames.
Live CF wire length is `0xacc`; both SPI2 DMA counts are `0x566`.

PCM is finite and non-silent before, during and after. Measured RMS values are
0.031389, 0.028748 and 0.028757, with zero silent windows among 415 transfer
segments (100 ms, except the final partial segment). WAV SHA-256:
`d5f82b77aaf920aa9430e081a27a9a835ac550632655f41bb46aab20a6cb3969`.
The unchanged daemon SHA is recorded in finding 92. Patched image SHA-256:
`f3598294ae81c7c1e4d06d5783888597b415f7e8bedf45c11b42adfde6de5d56`.
Independent review and artifact reconstruction found no discrepancies.

Validation: 31 focused tests, object/generation checks, scoped lint/types/format
and 195 host tests pass (16 skipped). Routed checks retain baseline path and
package-launcher failures (`out/lp0-v02-changed.log`); direct host evidence is
`out/lp0-v02-host.log`. Native logs are `out/lp0-v02-native-first-page-corrected.log`
and `out/lp0-v02-native-stock-audio.log`. No full merge gate is claimed.

**[O]** Exclusive buffer-gap and DMA-bank ownership, cache coherency, bounded
production send/recovery, generations and hardware timing remain open. Finite,
non-silent measured PCM does not prove sample-exact equivalence or glitch-free
hardware operation. The extension remains an explicitly opted-in diagnostic.
