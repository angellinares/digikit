# DN2 source-track identity and background dispatch

2026-10-29, DN2 OS 1.12. **[D]** inspected evidence; **[O]** remaining work.
This supports the [sampler plan](../plans/dsp-sdk-and-sampler.md). These are static
current-image traces, not a coupled voice-steal or hardware proof.

## Source-track identity

**[D]** The ColdFire frame builder `0x40025e36` supplies the current owner vector:
`0x40027594..96` copies the low halfword of each longword at
`0x80005308 + 4*v` into transmit-frame byte `0xa5a + 2*v`. The source is a
track key, not the physical voice index. Parameter-update helpers compare these
entries with track arguments.

**[D]** The parent found a computed owner store in the handler's decompilation;
a second agent checked the raw disassembly. After allocation through `0x4002a6a2`,
the handler iterates its returned slot mask and stores the event track value at
`0x40026a3c`. The indexed address simplifies from
`0x80003af0 + 4*(v + 0x606)` to `0x80005308 + 4*v`.
This assignment precedes the same invocation's frame construction. Unallocated
entries are not cleared in this path. No free-slot sentinel or complete upstream
track-range validation was established; consumers must reject owners outside 0..15.

**[D]** A separate original-byte SHARC audit places the packed owner map at
`0x257898`, with map write `0x1c2992` reading the preceding frame owner vector
at `0x25cee6 + 2*v`. This occurs before the current input copy at
`0x1c302d..33` and render-driver call `0x1c3044`. Therefore that map lags the
current frame. The candidate for new note binding is the current frame halfword
`0x52d + v`, with the adapter's existing one-block delayed note-on accounted for.
The source track must be latched for the note rather than reread as its bank owner
each render block.

## Implemented source-track input

**[D]** Runtime 0.5.3 supplies machine contract 1.1. `track` is appended at byte
16 of `dk_voice_in`; the original four offsets and ten-word V1 descriptor remain
unchanged. DT2 supplies the actual track. DN2 reads current frame halfword
`0x52d + slot` at the existing delayed note-on, rejects values at least 16, and
caches the owner in its per-slot adapter record. Every subsequent render sees
that cached track until retrigger. Invalid owners end only the slot, without
calling the module or incrementing its type-fault count.

**[D]** Seventeen assembled guard/track tests pass, including physical slot versus
track, owner changes during the silent arm block, cached ownership across render,
retrigger, invalid owners including 0xffff, other-slot recovery and DT2 tracks
0, 3 and 15. Eight focused adapter regression tests also pass. Existing four-field
fixtures keep working. Independent code review found no issue. Generation and
both adapter-object checks pass; rebuilt patchkit packs machine contract 1.1 and
runtime 0.5.3 into a new module. Logs: `out/track-guard-tests.log`,
`out/track-adapter-regression.log`, `out/track-release-checks.log` and
`out/track-patchkit-build-status.log`.

**[D]** Existing runtime loader and boot scanner reject declared machine minors
above the supported minor. This provides the compatibility gate for old runtimes;
new runtimes still accept older module minors. No new old-runtime coupled rejection
run was performed; the evidence here is the unchanged gate and actual packed
header. Adapter objects intentionally change to populate the appended field;
no SHARC core regeneration or PCM-pin change occurred.

**[O]** This input exposes identity, not a sample bank. Per-track bank generations,
selection/loading/publication and a coupled voice-reassignment proof remain
integration work. No hardware or complete sampler claim follows from these tests.

## Background-worker correction

**[C][D]** The SampleLoader task entry is `0x400f28e6`, not the earlier candidate
`0x400f28d0`. An independent raw-byte audit decoded the PC-relative constructor
instruction at `0x400f2884`; `0x400f28d0` is inside a reference-count destructor.
The actual entry initializes singleton `0x44648a20` and calls shared worker loop
`0x400f2498`. Queue entries are 0x24 bytes; the loop invokes the job function's
thunk at `0x400f2518`. Enqueuer `0x400f25f8` installs job/completion functions
and signals the worker semaphore.

**[D]** A concrete project operation exists on the ordinary BgWorker:
`0x4003fdee` queues `saveProjectToMmc(tempProject)` with invocation thunk
`0x4004029c`, which calls `0x40040190`. This is project persistence evidence,
not evidence for a DSP sample allocator. The SampleLoader accessor `0x4019eb1a`
is observed in a status/Loading-toast path at `0x4005e28a`; a concrete sample-job
producer remains unresolved. The bounded reference scan covered 99.94% of the
chosen code range but does not cover aliased/indirect object access. It cannot
prove the absence of sample-loading code.

**[D]** Raw ColdFire provenance: current section-3 SHA-256
`a098f620019b2ad38fd7e0397df39501fb1cf6bc26d0283915f34c49446fdf1a`.
The SHARC audit used section-7 SHA-256
`336e340aa0cdcd34e314cfa44849f709a3134f6bd4cd57dfc7e15702c83115e2`.
No firmware bytes or derived binary artifacts are committed.


Checkpoint checks: direct host tests pass 195 with 16 skipped across the initial
run and permission-corrected rerun of six local-socket tests (initial EPERM).
Scoped Python lint, generated-object formatting, document path checks and diff
whitespace pass. Routed `test:changed` retains baseline pathcheck and pnpm launcher
failures; no full integration gate or coupled playback proof is claimed.
