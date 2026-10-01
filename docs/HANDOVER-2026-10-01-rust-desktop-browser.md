# Handover 2026-10-01: Rust boot runtime, shared web faceplate, and desktop host

This is a cold-start handover for the current Rust ColdFire diagnostic runtime
and its Astro/Solid web and Tauri desktop front ends. It records what has been
demonstrated, what remains only a plausible diagnosis, and the shortest useful
next investigations. It does **not** claim an accurate full hardware emulator,
audio output, or synchronized real-time performance. See the October 2 follow-up below for
verified runtime improvements and their GUI-validation limits.

## Resume here: latest checkpoint and actual user goal

Follow-up on 2026-10-02: a freshly rebuilt matching DSP pack now renders
cleanly. Contiguous instruction metadata and eager host decode initialization
reduce the median first-block render stall from 6.97 to 1.18 ms in five
alternating native pairs; steady throughput is essentially unchanged. See
[`findings/07-emulator.md`](findings/07-emulator.md), **DSP decode allocation
and first-block performance follow-up (2026-10-02)**, for parity evidence,
commands and limitations. CPU/DSP synchronization remains open.

Current boot/UI follow-up on 2026-10-02: the shared runtime avoids timer scans
before a possible event and current-TCB reads before a matching frame return.
A fresh native boot/input sequence took 50.77 -> 34.45 s for DT2 and
43.47 -> 27.74 s for DN2. Intro frames now publish after a complete 8192-pixel
raster's last setPixel return, rather than sampling a cleared/partial bitmap.
Final native/WASM runs match all status values, five frame captures and all
87 intro revisions for both devices. Ten shared-library warnings were traced
to CLI diagnostics and cleaned up; they were not missing boot functionality.
See **Shared boot runtime performance, intro publication and warning audit
(2026-10-02)** in [`findings/07-emulator.md`](findings/07-emulator.md).
The browser public WASM asset and an external desktop release build are
updated; an already-running app/worker retains its old core. The user subsequently verified a smooth boot animation without flicker and
responsive UI after loading; cold boot still waits about 200M logical
instructions before the first visible logo.

Retained diagnostics follow-up (2026-10-02, after local commit `40731a0`):
**Export diagnostics** now downloads first milestones, counters, MAIN hash,
fault and host response timings. Optional PC sampling and a bounded event ring
are off by default; see **Retained boot diagnostics and audio integration
audit** in [`findings/07-emulator.md`](findings/07-emulator.md) for build flags,
measurements and proof limits. Portable per-access tracing is compiled out by
default; the diagnostic CLI retains it. Canonical native/profile/WASM status
and frame parity passes for both devices. A fresh DT2 native sequence took
32.76 -> 30.20 s; optional PC/events added about 2–2.4% in single-run comparisons.
The new default public WASM/static assets and external desktop release are
built. Relaunch/refresh to use them; no owner process was stopped.

MAIN-only boot remains the default. Bootstrap may supply a missing display or
DSP handoff, but this has not been established. Pre-intro PC samples concentrate
in buckets containing softfloat helpers (DT2 about 68%, DN2 about 63%). Keep
experimental softfloat opt-in pending differential proof. Neither current host
executes SHARC or outputs PCM; canonical replays recorded zero DMA frame
exchanges. The latest direct-SPI probe below verifies that DSP program traffic
is present. Fresh host silence remains missing integration.

Reset RAM-clear follow-up (2026-10-02, after `07b2f01`): verified complete
non-final iterations within already mapped pages now run as bounded host
fills. Guest counts/time, registers/CCR and original page allocation remain
reference-consistent. Native/default WASM full replay parity passes on both
devices; first native visible frame moves 9.02 -> 7.64 s DT2 and 9.52 ->
8.50 s DN2 in one paired replay each. Use the new
`ram_clear_fast_forwarded_instructions` counter alongside interpreted/idle
work. `reference-ram-clear` or detailed `diagnostic-trace` disables batching.
See **Guarded reset RAM-clear acceleration** in the findings for guards,
measurements and limitations. The user requested a separate checkpoint for these follow-up changes;
`git log` records that commit.

Final bounded boot optimization follow-up (2026-10-02, after checkpoint
`d68306c`): the outer timer-service gate reuses the existing cache before
Box ownership transfer. Complete native/WASM status, diagnostics and frame
parity passes on both devices. First visible frame moves 7.56 -> 6.39 s DT2
and 8.52 -> 7.76 s DN2 in one native paired observation each; guest counts
remain unchanged. Default WASM/static assets and desktop release are rebuilt.
See **Outer timer-service gate** in the findings for tests and timing caveats.

Fresh DSP/audio follow-up (2026-10-02): DT2 sends a byte-exact section-7
loader over direct DSPI2 and 69 LP0 slot-header transfers during no-card boot.
Added explicit generator decode ranges and `tools/sharc_reset_check.py`. Pure
native startup now matches all 7,543 completed Python reference instructions
before both stop on an unknown boot-source read at `0x10000000`. The stream's
earlier INIT callback at `0x120230` and ROM handoff context are not reproduced
by flattening loader memory. The official product datasheet now identifies
the stopped address as a normal-word DDR alias. The next implementation must
carry architectural address-space context through Python/native memory and
DAG modifiers; a blanket untyped alias is unsafe. A bounded pre-INIT
calibration also identifies static EMUCLK and PLL status as dependencies.
See `docs/refs/adsp-2156x-data-addressing.md` and the findings for evidence.

Separately, a three-second existing captured-state SHARC speaker test succeeds
at 48 kHz stereo with zero underruns/stops. Fixed a real device-name query
panic in the native player. This verifies rendering and the sink; the fresh
desktop/browser runtime still needs coupled DSP/audio integration. See
**Fresh DSP traffic, native startup frontier and speaker proof** in findings 07
for commands, evidence and limits.

**The goal is synchronized ColdFire + SHARC execution close enough to device
real time to sustain audio. Boot/UI speed is secondary.** The user stopped the
boot-focused work and explicitly requested committing all current source/docs
changes with this handover for a fresh context. Obtain the checkpoint hash
with `git log -1`; further commits still need user approval.

Latest observations and evidence are recorded in
[`findings/07-emulator.md`](findings/07-emulator.md), under **Performance and
real-time audio checkpoint**. Current implementation state:

- PIT/DTIM service-loop clones were removed. `Board` now retains
  `Option<Box<Time>>`, preserving timer detachment around callbacks. Four tests
  cover stable ownership, MMIO, pending IRQs and explicit error restoration.
  Matched native/WASM reference captures passed for both devices. Observed
  native DT2 QA moved from about 119 to 66 seconds; no audio claim follows.
- `native/boot/src/softfloat.rs` adds **opt-in** `SoftfloatAbiV1` add/mul/div.
  `Emulator::new`/`digi_load` remain reference; use
  `new_with_policy(..., ExecutionPolicy::SoftfloatAbiV1)` or the explicit
  `digi_load_softfloat_abi_v1` export. No frontend selects this mode.
  It substitutes ABI result/return state, not exit CCR/scratch/stack or exact
  interrupt timing. Each accepted call costs one synthetic Oracle tick and
  does not increment `cpu.icount`. Exceptional/zero-result cases defer.
  `Board::ram_matches` checks live `[add, cmp)` bytes without copying 1 MiB
  pages; division rounds directly in binary32. Complete routine/dependency
  coverage and per-routine differential checking remain open: prefix bounds
  alone are not sufficient evidence. The softfloat-only DT2 pilot booted and
  accepted input but still took about a minute to readiness.
- The shared chunk runner analytically advances verified `BRA.B`-to-self
  idle passes, strictly before timer deadlines, 20k rescheduling passes and
  chunk end. Guest clock/pass counts are retained and actual interpreted vs
  analytic idle work are separate counters. The complete DT2 reference
  ready/two-NO/encoder sequence matched all five binary captures and all legacy
  ready/final JSON values. Additional accounting fields are intentional.
  **DN2/current WASM and actual GUI validation were not repeated for idle or
  arithmetic changes.** Thirteen boot-library tests, the RAM-comparison test
  and active changed-path LSP probes passed before the checkpoint checks.
- `icount` includes flash substitutions and analytic idle work, not solely
  host interpreter work. `oracle_ticks` additionally includes accepted atomic
  arithmetic calls. Never compare synthetic/logical counts as interpreted
  MIPS; measure audio-block/wall time and report the distinct counters.
- Main frames are task/return-completion-gated; unchanged revisions are
  suppressed and main latches over intro. The October 2 follow-up replaces
  intro chunk sampling with complete-raster return capture. Complete black
  frames remain publishable; no substitute images are drawn.

### Actual audio path and immediate blocker

Parallel source scouts found the existing Python GUI `--live-audio` path feeds
ColdFire DSPI2 frames to real native SHARC instruction execution. However,
`emu/livesharc.py` forces SSI0/vector 191, returns zero DSP RX bytes, and repeats
commands between incoming frames. Audio-ring/device pacing and CPU instruction
pacing are independent: **there is no unified device-time synchronization**.
Generic `dsp=True` is only a ready/FIFO stub. The new Rust desktop/browser boot
paths do not yet connect to `native/live` or SHARC.

The native DSP renderer already uses generated AOT blocks, so do not assume
another JIT is the answer. Existing device-free baseline:
`native/live/target/release/live_play --pack PACK --lib LIB --bench 100
--times /private/tmp/FILE`. Calls have 64-instruction DMA and 4,000,000-instruction
block limits. At the existing requested 48 kHz and 32 samples/block, nominal
output budget is 0.667 ms; this does not establish hardware cadence or worst-case
polyphony. The renderer taps voice work buffers, not automatically a complete
firmware DAC/master-FX output.

The first 100-frame attempt **refused a stale pack before rendering**:
`out/native/live/v3/59095b644e12a4e3656c70b5.pack` records core `124ec4db...`,
while current native/core hash is `4b25379c...`. Rebuild a matching pack from
the original capture/FlexBus inputs using `tools/sharc_transpile_run.py
live-pack`; inspect existing `--rebuild`, `--limit` and `--out-dir` options.
Do not bypass stale guards or merely retag old state. Captured-state rendering
is an explicitly separate benchmark, not a substitute for full emulator boot.
No DSP throughput, underrun or coupled real-time result was obtained here.

### Evidence and operational traps

- Latest scratch evidence: `/private/tmp/digi-acceleration-1yDSzAfs/`, notably
  `reference-idle-isolated-receipt.json`, `dt2-native-pilot/report.json`,
  `sharc-100-receipt.json`, `sharc-100.log` and `meta/*`.
- Scoped timer proof: `/private/tmp/digi-box-timer-main-ujmZRJAi/`.
- Audio source reports: session subagent artifacts for workflow
  `c875ac3f-50b2-4485-8995-b1a1b22fa5d2`, under `real-time/`.
- These paths are temporary and may be OS-cleaned. Check facts in the committed
  findings if scratch files are absent.
- Port 4321 is the owner's Astro dev server. Its observed WASM matched the
  generated public asset but differed from scoped QA builds: a provenance
  caveat, not proof it is stale/slower. No owner process/server was stopped.
- Use Rust 1.98.1, locked/offline Cargo and external targets. Give temporary
  harnesses with the same package/binary name **distinct target directories**:
  shared pilot/canonical targets selected the wrong executable once. The
  corrected isolated run passed; the initial CLI panic did not run firmware.
- `tools/native_wasm.sh` now copies from the actual `CARGO_TARGET_DIR`. Its
  `--diagnostics` flag enables PC/events; running without it restores default.
  Record the actual public/static build hashes and refresh existing workers.
- Automatic mutators earlier reintroduced unrelated SOFF/state edits. A
  temporary `.pi-lens.json` paused mutations only; diagnostics stayed enabled.
  It is not a permanent config and must not be committed. Preserve the original
  `i32::try_from(tcd.soff).expect(...)` and `raw.chunks_exact(...)` expressions.
  Empty/stale diagnostic caches do not prove source cleanliness.
- Timed emulator/audio runs remain serial, <=300 seconds each, with guest
  instruction/frame/chunk limits and verified firmware provenance. Scouts may
  read in parallel; keep one writer per tree. Native child completion wakes
  exist; no polling/sleep loops or silent external-agent fallback.

### Next context: shortest useful order

1. Verify checkpoint/worktree and original config absence; do not disturb owner
   processes. Finish current pack provenance before any DSP measurement.
2. Rebuild a matching DSP benchmark pack and run the bounded native 100-frame
   device-free benchmark. Report clean/stopped/DMA failures, cold/steady
   median/p99/max, interpreter fallback and the explicitly nominal block budget.
3. Define the shared CPU/DSP virtual-time contract and SPI reply/SSI0 handoff.
   Separate missing integration from execution bottlenecks. Measure sustained
   coupled block deadlines/underruns with representative polyphony and inputs.
4. Optimize only demonstrated audio-relevant bottlenecks. Parallelize distinct
   component investigation or isolated scoped writes, not competing timed runs.
   Parent owns architecture/acceptance; avoid another broad discovery council.
5. Before frontend arithmetic activation, finish differential, DN2/current WASM
   and real-GUI checks. DT2 reference parity is not full hardware/audio proof.
   Treat intro flicker as a separate observable publication problem.

The older sections below preserve original integration/launch details; this
resume section supersedes their historical performance estimates and priority.

Checkpoint checks: 219 Rust tests passed across periph/machine/boot/PlusDrive/
device-profile (13 existing ignored tests remain ignored). Both changed Python
instruction-pin tests passed with `--slow` (48.83 seconds). Astro check reported
zero errors and one hint. Desktop and current WASM Cargo checks, changed-file
Rust formatting and Python lint/format checks passed. The initial bare-compiler
WASM check selected the wrong sysroot; explicitly using rustup's pinned
`RUSTC`/`RUSTDOC` resolved it without installing or changing dependencies.
Check logs are in
`/private/tmp/digi-handover-commit-checks/`. The full Python/slow suite and real
GUI/audio playback were not rerun for this checkpoint; prior full-suite caveats
below remain relevant.

## Working-tree state and scope

The branch is `work/sharc-emulator`, currently based on checkpoint `187c066`
(`Extend native emulator boot and add Rust PlusDrive and web faceplates`). That
commit added 58 files and 14,269 lines before this integration work. The
current integration is included in the source checkpoint explicitly requested
by the user. `git status --short --branch` and `git log -1` are the source of
truth before follow-up. Do not reset/discard work or make further commits
without authorization.

Firmware, `out/`, `.claude/`, worktrees, and three local handovers are local
artifacts, not commit candidates. SQLite persistence is explicitly deferred.
The end goal remains full CPU-and-DSP emulation for both devices with desktop
and browser audio. The current implementation is a bounded diagnostic toward
that goal: it boots supported real MAIN OS images, displays firmware UI,
accepts a limited panel-input path, and keeps native and WASM behavior
comparable. It is not a claim that every peripheral or firmware version is
supported.

Working rules for follow-up: use personal scouts for reads and workers for
small scoped writes; the root agent owns complex decisions and integration.
Use fresh contexts, no nested children, and one writer. Read applicable project
instructions and use the harness's current delegation contract; do not bypass
hooks or silently switch execution protocols.

The two supported contracts are Digitakt II OS 1.16 and Digitone II OS 1.11.
The registry verifies SYX layout and the MAIN SHA-256 at runtime; firmware is
not embedded in the WASM. Existing 1.15C and 1.10E files are deliberately
outside those contracts and should fail with a concise unsupported-contract
error rather than attempt a near match. Contract and device definitions are in
[`native/device-profile/src/lib.rs`](../native/device-profile/src/lib.rs) and
[`devices/`](../devices/).

## What was built

The portable runtime is exposed by [`native/boot/src/lib.rs`](../native/boot/src/lib.rs).
[`common.rs`](../native/boot/src/common.rs) holds shared parsing/support code,
[`runtime.rs`](../native/boot/src/runtime.rs) owns the emulator state, and
[`abi.rs`](../native/boot/src/abi.rs) provides a small raw WASM ABI
(`load`, `step`, input operations, and `stop`) without `wasm-bindgen`.
`Emulator::new` requires SYX bytes and takes an optional card source;
`step_chunk` has a 250,000 bound on service iterations (which include
service-only iterations, so it is not a guaranteed instruction count), and
returns a persistent fault in its status rather than hiding it. `Status` and `Snapshot` carry the
optional 1024-byte frame, a frame revision/source, readiness, input state, and
filesystem status.

The board boot path preserves the firmware's MAIN entry path and models the
parts needed by the observed boot: PPMCR, UART8, DSPI, forced statuses for
known Oracle MMIO registers, bounded zero handling for unknown MMIO,
virtual PIT/DTIM timers at a 132M guest-instructions-per-second scale,
TX35 completion, ELE3 flash HLE, sparse SDRAM, an idle trap every 20,000
instructions, UART8/TCD34 panel transfer, VBR 154 RX IRQ, panel input queues,
and canonical device profiles loaded from TOML. Held keys are cumulative and
queues are bounded. `input_ready == false` before the first queue probe is
expected; do not disable keys just because it is initially false.

The plus-drive implementation lives in [`native/plusdrive/`](../native/plusdrive/).
A no-card run gets a formatted empty Rust PlusDrive; empty-root/list handling
was corrected. `tools/native_plusdrive.sh` can build/inspect an image, and the
emulator accepts `--card-image FILE` as a read-only external overlay. A
Digitone II `filesystem_verified: null` means no filesystem check has been
established for that device; it must never be reported as a verified FS check.

The browser runtime is an actor-style Web Worker in
[`packages/web/src/runtime-worker.ts`](../packages/web/src/runtime-worker.ts).
[`packages/web/src/runtime.ts`](../packages/web/src/runtime.ts) contains both
the worker adapter and a Tauri IPC adapter. The desktop adapter owns the
non-`Send` `Emulator` on a standard thread, sends work through `mpsc`, and uses
`spawn_blocking` to receive it. It protects replacement/retry/pause/input with
generation tokens, session IDs, and an epoch; raw firmware bytes use the
`x-session-id` header. The Tauri host is in
[`packages/desktop/src-tauri/src/main.rs`](../packages/desktop/src-tauri/src/main.rs).

The front end uses shared Astro/Solid assets and the same faceplate data for
both browser and desktop. The latest WebKit-specific layout repair matters:
the OLED canvas was moved **outside** the SVG, into the HTML
`.panel-surface` overlay, with normalized coordinates from the 215 by 176
faceplate viewBox. `foreignObject` canvas placement produced a visible WebKit
bug where user pixels appeared beneath the headphone area. The latest user
screenshot confirmed the OLED placement during the intro. The relevant code is
[`packages/web/src/components/Faceplate.tsx`](../packages/web/src/components/Faceplate.tsx)
and [`faceplate.css`](../packages/web/src/components/faceplate.css).

## How to run it

Use the pinned toolchain in [`mise.toml`](../mise.toml): Rust 1.98.1 and pnpm
12.8.2. From the repository root:

```sh
mise run boot -- Digitakt_II_OS1.16.syx
mise run emu -- Digitakt_II_OS1.16.syx
mise run emu -- Digitone_II_OS1.11.syx --card-image /absolute/path/card.img
pnpm dev
pnpm --filter @digi/desktop build
```

`mise run boot -- SYX` is the bounded headless diagnostic; inspect
`tools/native_boot.sh` for its exact options rather than guessing wrapper
arguments. `mise run emu` builds the shared static web panel and runs the
desktop diagnostic; with no argument it opens its file picker. The Tauri build
uses the web build through its `beforeBuildCommand`; the expected macOS bundle
is `packages/desktop/src-tauri/target/release/bundle/macos/Digiemu.app`.

The browser's `predev` and `prebuild` both run
[`tools/native_wasm.sh`](../tools/native_wasm.sh). It deliberately discovers
the rustup-selected compiler, exports `RUSTC`, `RUSTDOC`, `DYLD` fallback and
the matching sysroot: invoking the bare mise compiler previously selected a
Homebrew compiler with the wrong standard library. It needs the pinned
`wasm32-unknown-unknown` target and cached Cargo dependencies because the build
runs offline. Build the WASM first if these prerequisites are absent; do not
weaken `--offline` as a casual workaround.

## Verification already performed

The strongest parity check uses an external native harness and a Node raw-WASM
harness with matching 250,000-iteration chunk requests. For each supported
device it waits for diagnostic readiness (at which point `input_ready` remains
false), sends uppercase `NO` at ready and releases it at +1M, sends a second
`NO` at +20M and releases it at +21M, turns encoder A at +40M, then stops at
+60M. Native and WASM produced identical `ready.json`, `status.json`, and all
five captured 1024-byte framebuffers for both contracts.

| Device | ready instruction | final instruction | ready frame revision | filesystem result | visible path |
| --- | ---: | ---: | ---: | --- | --- |
| DT2 1.16 | 914,736,053 | 974,985,741 | 797 | `true` | `FILESYSTEM OK` → SLC warning → `UNTITLED` source → A parameter 127 |
| DN2 1.11 | 847,486,347 | 907,736,131 | 794 | `null` | SLC warning → main SYN page; A turn has no visible change |

Both final states had five input IRQs, no pending input, and no emulator error.
This proves the specified bounded state/input sequence is parity-consistent; it
does not prove audio or unrestricted long-running correctness. Native ran at
roughly 8 MIPS and Node WASM around 7 MIPS in that QA, taking about 120–149
seconds. Runtime artifacts exist in `/private/tmp/digi-runtime-{dt2,dn2}-20261001`
and `/private/tmp/digi-wasm-{dt2,dn2}-20261001`; harnesses are
`/private/tmp/digi-runtime-qa-20261001` and
`/private/tmp/digi-wasm-qa-20261001.mjs`. They are reusable evidence, but `/tmp`
is not durable project storage.

Focused tests previously passed: 19 boot tests, 7 device-profile tests, and
16 PlusDrive tests. Astro check, production/WASM build, and bundle build
passed after the latest OLED repair. The desktop `cargo check` was earlier,
with no Rust change in that repair.
The existing baseline Python suite last reported 2,095 pass, 29 skip, 1 xfail,
and 7 failures. All seven were investigated: two golden tests held stale
metadata; three native guards were regenerated after the SHARC library source
fingerprint/generated-output change and 15 focused guard/transpile tests passed; two instruction
pins changed only in their explicit expected counts. The focused pin tests
passed (205,769→205,747 replay and 407,044→407,022 capture) in 49.74 seconds;
the full 19-minute suite has not been rerun. The current DM Type 15b read is at
`b819fa`: R2 remains 16 in both runs; old S2 was stale 486832 and the correct
adjacent-word companion load sets S2 to 16. XAZ1 is true in both, YAZ changes
from 0 to 1, and the SIMD Type8a
`b81a12` AND condition now skips 22 PCs (`b81a15..b81a3f`). Other replay fields
match. Do not silently roll back those pins.

The native-adapter mock at `/private/tmp/digi-native-adapter-qa-20261001.mjs`
passed failure/retry, stale-step, pause/epoch, and input-replacement cases.
An earlier production browser check reached DT2 main UI and physical NO cleared
dialogs without errors. An earlier actual DN2 browser run produced
`/private/tmp/digi-browser-dn2-20261001.png`; its Chrome layout was fine but
its WebKit `foreignObject` layout was the issue fixed later. Tauri CLI startup
also showed the 150M counter with Pause/Restart enabled.

Use the evidence at the right strength. Equal native/WASM JSON and framebuffer
captures are deterministic runtime evidence. A visible browser screenshot is
UI evidence for the single captured state. A toolbar/control observation is
host lifecycle evidence. Neither substitutes for an audio assertion, a timing
profile, or a long-run hardware-compatibility result. In particular, expected
startup messages are `Booting — waiting for display` until the first frame and
`Booting — startup display` for an intro frame; a 0-pixel canvas before roughly
240M instructions is expected for the current actual boot path, not enough by
itself to label the UI broken. The latest user picture confirms the repaired
WebKit OLED position at intro instruction 242,999,571, while slow/flickering
animation remains.

## Current limits, likely diagnosis, and next work

Performance is still inadequate for smooth desktop/browser use. In an actual
browser, early pixels appear around 240–254M instructions, main UI around
843M, and DT2 readiness around 915M; observed browser speed is about 5–6 MIPS.
This is not a complete performance characterization. The old Python GUI enables
HLE and snapshot/unblock work in `emu/gui.py:401`, holds PIT/DTIM during the
intro before handover (`emu/gui.py:465,489`), and uses prepared snapshots (see
`emu/run.py:270`). Its Unicorn fast runner calls `emu_start` in 200k chunks
(`emu/fastrun.py:3,67`). `emu/longrun.py:131` is historical documentation that
describes softfloat as about 93% of instructions and reports 2M→20M/s with HLE;
it is not a current Rust performance result.

The Rust path interprets ColdFire one instruction at a time and runs PC checks,
observers/queues, CPU stepping, and timer service in
`native/boot/src/runtime.rs:408–517`; its timer state also moves/restores and
writes host events in `common.rs:780–847`. Board reads cascade through
`native/machine/src/board.rs:903–931,1088–1140`. Do not begin by replacing the
CPU decoder or flags: it already has a decoded-instruction cache
(`native/coldfire/src/cpu.rs:122–145,503–539`) and lazy flags (`223–244`). Rust
SHARC has generated AOT block execution, but that does not accelerate this
ColdFire interpreter. These are candidate contributors, not a profile-backed
root cause.

There is an existing separate ColdFire microbenchmark in
[`docs/plan-native-emulator.md`](plan-native-emulator.md) (lines 91–94): its
real-mix result is 80.8–82.2 useful MIPS with state hash
`0x0bb544e65d49266b`. It explicitly excludes a full machine and browser, so it
is not comparable directly to raw boot instruction counts. The benchmark
(`native/coldfire/src/bin/cfrealmix.rs`) uses a snapshot/flat-page-pointer stub
bus, returns zero for unknown peripheral reads, drops writes, has no interrupts
or MMIO trace, and times CPU steps excluding idle spins. Reproduce it only as
a separate CPU baseline:

```sh
python tools/cf_snapdump.py snapshots/dt2-1.16-drive3/boot280M.snap out/coldfire-bench/boot280M.cfdump
cd native/coldfire
cargo build --release --bin cfrealmix
target/release/cfrealmix ../../out/coldfire-bench/boot280M.cfdump 100000000
```

The new boot crate does not depend on `native/sharc` or `native/live`; the
desktop crate depends on boot, Tauri, and serde. The new ColdFire boot app has
therefore not been connected to the old SHARC/live audio path. Treat the
microbenchmark and prior SHARC results as separate evidence.

The observed flicker is also not fixed. A high-confidence hypothesis is that a
mutable intro BMP is sampled at a chunk boundary, exposing a clear or partial
draw: the Rust refresh path samples the current BMP and its header at
`runtime.rs:594–610,873–897` during chunk refresh, while only the main-frame
tracker has a completion return boundary (`common.rs:298`). The old Python GUI
publishes after its `setPixel` coordinate cycle repeats (`emu/gui.py:347,349`).
Capture frame revisions and draw-completion timing to confirm the causal chain
before changing behavior. Do not globally suppress black frames, add artificial
delays, or draw substitute images.

The newest daemon/browser attempt after restart failed before the component was
mounted and its input node became available. It is an inconclusive environment
test, not evidence of a source bug. The latest canvas geometry plus full boot
has therefore not been rerun in a real browser/WebKit session. The screenshot
validates placement only.

After the handover was requested, the root stopped only its QA headless server
on port 9333 (PID 16070) and static server on port 4323 (PID 26912). No user
session or application was stopped; the current process check found no
`Digiemu` process. Do not describe the desktop app as currently running after
that daemon pause.

The original boot/UI-first work order is superseded by **Next context:
shortest useful order** above: the acceptance target is sustained synchronized
ColdFire/SHARC real-time audio, not another boot MIPS/relative-speedup claim.

Before declaring the UI repair complete, rerun a current real-browser/WebKit
full boot for DT2 and DN2, retain screenshots/status/frame evidence, and repeat
the production desktop lifecycle checks. Keep claims limited to the documented
diagnostic paths until those results and profiling exist.
