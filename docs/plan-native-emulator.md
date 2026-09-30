# Plan: a firmware-agnostic native emulator (desktop and browser)

Status 2026-09-30, current HEAD `39f8f20`; the original plan base was
`95d92de` on `work/sharc-emulator`.

## Goal

A user selects their own firmware file (`.syx`) and the emulator runs it:
ColdFire and SHARC together, in real time, with audio, on the desktop and in
the browser. Targets: Digitakt II OS 1.16 and Digitone II OS 1.11 (same
architecture; the differences are board and firmware level). DSP patching
(the original goal) builds on it.

Constraints:
- No firmware or firmware-derived data in the repository or in anything we
  distribute. All per-image work happens on the user's machine at load time.
- Every native component is checked against an existing reference (oracle)
  before it replaces anything.
- Cores are pure: no OS calls or threads inside a CPU core. OS-specific code
  (audio, files, threads, windows) stays at the edges.

## Revised priority (2026-09-29, after stage 2)

Real time on the desktop is limited by the ColdFire, not the SHARC: the
ahead-of-time SHARC core already renders a frame in 411-467 us of 667 with
0 underruns, while the ColdFire runs at 0.22x (Unicorn plus Python hooks
cannot pass about 0.4x). So:
1. Pause the SHARC JIT (P2) until the browser phase (P6); it is correct and
   deterministic but 2.5-3.3 ms/frame, and only the browser and patched
   images without a rebuild need it.
2. Gate: the Rust ColdFire interpreter's speed on a real instruction mix
   (memory and peripheral dispatch, idle skipped) against about 62M useful
   instructions/s. No ColdFire JIT on the desktop if it passes.
3. Remaining peripherals against the traces: eSDHC and card, GPIO, UART,
   panel input, display.
4. P5 machine wiring: ColdFire core + native/periph + the ahead-of-time
   SHARC + native/live in one native process, checked against the Python
   emulator on the same snapshot windows, then 1.0x with audio.

## Hand-off: status and next tasks (2026-09-29, commit fc15de5)

Self-contained for a new session or another model. Branch
`work/sharc-emulator`. Rules: CLAUDE.md, plus the working method below.
Always `DT2_SYX=Digitakt_II_OS1.16.syx`.

### Status

| Area | State | Where |
|---|---|---|
| Live play (desktop) | Works: a TRIG in the GUI plays through the ahead-of-time SHARC core. The ColdFire (Unicorn + Python) runs at 0.22x, so the UI and sequencer are slow. | `uv run python tools/dt2gui.py --live-audio`; finding 15 |
| SHARC, ahead of time | 411-467 us per frame of 667, exact vs Python. The library carries a `sharc_core` hash and refuses a stale build. | `native/sharc`; rebuild command = `REGENERATE_HINT` in `tools/sharc_transpile_run.py` |
| SHARC JIT (P2) | Paused. Correct (91/91 drive3 frames) and deterministic, but 2.5-3.3 ms/frame under wasmtime. On resume: diff its WebAssembly for one hot loop against the ahead-of-time code compiled to wasm32 (630-720 us/frame), fix the differences, and measure with counters (region entries/frame, bytes/instruction, spills). | `native/sharc-jit` (build with `SHARC_GEN_DIR=$PWD/out/native/opt/gen-final`) |
| ColdFire core (P3) | All 101 used forms; fuzz and EMAC lockstep pass, and bounded DT2 snapshot windows pass under the known Unicorn MVZ-N exemption. The decode cache reaches **80.8–82.2M useful instr/s** with the accepted state hash. Broader windows, DN2 exception boundaries and oracle-exempt EMAC behavior remain open. | `native/coldfire`; `tools/cf_lockstep.py {snap,fuzz,emac}` |
| Peripherals (P4) | Timers, INTC, eDMA, SSI0, DSPI and DSP FIFO have replay gates; focused early GPIO/eSDHC/card traces and bounded channel-59 transfer effects also pass. Broad peripheral coverage and real trace gates remain for late DMA delivery, UART, panel and display. | `native/periph` (`mmio-replay <trace> [--limit N] [--verbose]`); traces in `out/mmio-trace/` from `tools/mmio_record.py` |
| Machine (P5) | Partial: `native/machine` has `Board`/`Time`/`Runner` Oracle-replay probes. Autonomous Device timer delivery is deliberately rejected, and the crate has no SHARC/live-audio integration yet. | `native/machine`; `docs/design/p5-machine.md` |
| Unicorn oracle | 5 patches installed (`tools/install-patched-unicorn.sh`). Four known EMAC defects outside the firmware's MACSR modes (0x00, 0x20) are not patched, and the MVZ N flag is wrong; `cf_lockstep` treats these as oracle-exempt. | `patches/README.md` |

### Benchmarks and gates

- ColdFire speed:
  1. `PYTHONPATH=. uv run python tools/cf_snapdump.py snapshots/dt2-1.16-drive3/boot280M.snap out/coldfire-bench/boot280M.cfdump` (once);
  2. in `native/coldfire`, `cargo build --release --bin cfrealmix && ./target/release/cfrealmix ../../out/coldfire-bench/boot280M.cfdump 100000000`.

  Gate: the state hash stays `0x0bb544e65d49266b` and instructions/s are reported. (That dump is byte-identical to the one the baseline used.)
- ColdFire correctness: `uv run python tools/cf_lockstep.py fuzz dt2-1.16 --seed 42` (and `dn2-1.11`), `... emac`, `... snap SNAP --limit N`.
- Peripherals: `cargo run --release --bin mmio-replay -- out/mmio-trace/<trace>.mmio` in `native/periph`.
- Tests: `uv run python -m pytest tests -q` (add `--slow` before a commit; the SHARC slow tests need splitting into groups under 10 minutes) and `cargo test --release` in each crate.

### Next tasks, in order

This is the 2026-09-29 hand-off list; completed items are superseded by the
progress and migration gates below.

1. **ColdFire divergence at 0x401768a6** (oracle-driven; Sonnet or GPT). Find which side is wrong using the manual, fix it, then run `snap` lockstep over more windows of both images (the drive3/auto snapshots and `snapshots/dn2-1.11/`) until they are clean or every exemption is explained.
2. **ColdFire handler dispatch** (Sonnet or GPT): one handler per decoded form with pre-extracted operands, no `Result`/`dyn` on the hot path. Gate: at least 62M instructions/s on `cfrealmix`, hash unchanged, lockstep unchanged. If it falls short, profile before considering a ColdFire JIT.
3. **Remaining peripherals** (oracle-driven): eSDHC + card (with the +Drive overlay), GPIO, UART, panel input and display, each against the traces and following the `native/periph` interface (see its lib.rs docs).
4. **P5 machine**, following `docs/design/p5-machine.md` step by step. Opus for the scheduler, the oracle/device interrupt delivery and the pacing; the rest is oracle-driven.
5. **Findings to record** (with a second check before [V]):
   - the four Unicorn EMAC defects and the MVZ N flag;
   - Ghidra's MAC-with-load mis-decode at DT2 0x400d92e2 and DN2 0x400db16a;
   - the registers the Python emulator leaves as RAM (INTC force/mask, DTIM1, eDMA CERQ, DSPI2 MCR, DN2 edge port).
6. Later: resume the SHARC JIT (P2, method above), then the browser (P6) and DSP patching (P7).

### Progress after the hand-off (2026-09-29)

- Task 1: the `0x401768a6` divergence in `snapshots/boot280M.snap` is
  Unicorn's MVZ N-flag defect, not a Rust core error. The bounded exemption
  permits 30,000-instruction DT2 windows; DN2 still exits the harness at an
  unmapped/exception boundary after 1,477 instructions. Broader windows of
  both images remain to be checked. See finding 07.
- Task 2: the interpreter's tagged direct-mapped decode-page cache passes
  the real-mix floor at 80.8–82.2M useful instructions/s (formerly 47.0M),
  with the same `0x0bb544e65d49266b` state hash. A full native machine and
  browser benchmarks are **not** covered by this gate. Next: task 3 and P5.
- The original boot MMIO traces start from late snapshots; they contain
  GPIO/eSDHC traffic but not a complete cold-boot GPIO/card-init gate. Do not
  treat their replay as proof of complete card behavior. Record an earlier
  bounded oracle window before implementing that gate.
- Task 3 (partial): early 24M checkpoints now supply bounded DT2/DN2 traces
  through the GPIO SD gate and initial eSDHC access. `native/periph` matches
  all 20 gate writes, 20 read-hook writes and 20 reads per image in its
  explicit focused replay mode. Original v1 whole traces report 2/3
  unexpected vector-207 IRQs because they cannot see a guest SR write after
  RTE. New v2 traces sample SR before timer service; both whole early
  windows now replay with zero mismatches, including the GPIO/eSDHC gates.
  The early eSDHC register slice checks 74 accesses and 111 host writes per
  image; pure eMMC and bounded channel-59 CMD8/18/25 transfer effects build
  for WASM. A machine-facing adapter also joins the real card, caller-owned
  RAM, staging and live TCD writeback under synthetic tests. Late SERQ59
  dispatch, physical RAM mapping, DMA completion delivery, and
  UART/panel/display remain. See finding 07.

### Migration gates (2026-09-30)

1. Preserve the accepted native/Oracle replay gates while resolving sustained
   native SHARC state and control correctness. Recorded AOT SHARC performance
   is not a claim about JIT misses or browser execution.
2. Validate a native Device timer/IRQ delivery contract before enabling it.
   `TimerPolicy::Device` is reserved (`native/machine/src/time.rs:21-36`) and
   rejected by `Runner::step_timed` (`native/machine/src/runner.rs:278-289`);
   Python callback-entry counts are not its oracle.
3. Integrate the native path in order: ColdFire frame production, DSPI wire,
   rendered inputs, SHARC, then PCM. Gate each seam against its recorded
   inputs and state before claiming integrated audio.
4. Keep an independent wasm32 compilation gate for the native cores and
   machine. The locked offline checks now pass for `coldfire`, `periph`
   (without default features), `emmc-card` and `machine` when Cargo uses the
   rustup toolchain that supplies wasm32 std; this is not a linked, runnable
   browser module or full emulator. The generated AOT SHARC wasm-frame module
   also passes this compile gate on the latest sources: 12,197,477 bytes,
   SHA-256 `720efe510b9ef028a32e8831bdbbbce91e6ab20c0ebd86dbb6e7b4a40858493d`,
   built in 45.45 seconds with the rustup 1.98.1 toolchain,
   `DYLD_FALLBACK_LIBRARY_PATH` set to its `lib`, `SHARC_GEN_DIR` set to
   `out/native/opt/gen-final`, and wasm flags `-C link-arg=-zstack-size=8388608`
   and `-C target-feature=+simd128`. This is an AOT/generated frame module,
   not a SHARC JIT performance result or integrated browser emulator.
5. **Browser shell:** the user-provided Astro/Solid/pnpm single-page
   faceplate design is accepted and its shell is implemented. Its runtime is
   not connected. Firmware boot in a browser, AudioWorklet/realtime behavior,
   and browser JIT remain separate gates.

## Where we are

| Part | State |
|---|---|
| SHARC semantics | `tools/sharc_core` is the source of truth. Current bounded state-pack gates require regenerating the initial pack after a semantic change. |
| Native SHARC | The current 204/256 bounded replays are exact state/stop gates, not full instruction-parity or realtime claims. Historical AOT timing and compile-only modules remain separately qualified. |
| Live audio | `native/live` (cpal); `emu/gui.py --live-audio` plays TRIGs live. |
| Current live ColdFire path | Unicorn with 5 patches plus about 30 Python peripheral models and hooks. 0.22x real time with the audio clock running; about 0.4x is the ceiling of this design. The Rust ColdFire core and native-machine probes are recorded separately in the current status table; they are not yet wired into this live path. |
| Oracles available | Python SHARC core, `tools/sharc_diff.py` lockstep harness, patched Unicorn, the Python machine, `.snap` snapshots, `tools/snapeq.py`, `.dt2cap` captures, the FlexBus log. |

## Lessons from this round

1. **Do not transpile general Python again.** The Python-to-Rust transpile of
   `sharc_core` was exact on the first pass, but it carried Python's
   generality into Rust: 128-bit integers with known-bit masks, an undo
   journal, `Result` on every call, and 399,470 block variants (166 MB of Rust,
   a 10-minute build, 24.6 ns/instruction). An optimisation lane had to remove
   all of that to reach 1.9 ns. Keep that transpiler for the SHARC, where the
   core is large and now fast. For new components, write Rust directly with
   concrete types, and generate only from data (decoder tables from
   manuals), not from code.
2. **Recompiling the firmware ahead of time ties the build to one image.** A
   JIT that translates blocks at run time from the loaded image keeps the
   speed and removes firmware from every build output.
3. **Re-typing verified code is the most expensive step.** A spec written
   by a design agent and applied by a coder cost 9 coder resumptions for 155
   blocks, plus two paraphrase errors caught only by byte comparison.
   Copying the verified files from the worktree is exact and nearly free.
4. **Worktrees must start from a commit.** Syncing uncommitted work into
   worktrees caused one stale-file overwrite and needed three-way merges.
   Commit at every phase gate.
5. **Unseen code paths fall back to slow paths.** The AOT build covers only
   recorded blocks; the live path single-steps about 400 instructions a frame
   because the coverage was recorded before the timer changes. A JIT removes
   the coverage step.

## Working method (token and time budget)

- **Oracle first:** each lane starts with the differential test against its
  oracle, then implements until the test passes. The test is the
  specification; agents do not read Python models line by line when a trace
  answers the question.
- **Roles:**
  - A **scout** reads and answers with file:line quotes.
  - A **deep** agent decides, implements and verifies in a worktree that
    starts from the phase's commit.
  - Merging copies the verified files and checks them byte for byte.
  - A **coder** is used for small hand-specified edits only.
- **One owner per file:** lanes own whole crates or files, and shared lists
  (`tests/test_lint.py`, `tests/test_types.py`, `CLAUDE.md`) are edited only at
  merge time.
- **Reuse the tools:** `tools/cf.py` (cfdb), `tools/snapread.py`,
  `tools/sharc.py`, `tools/sharc_diff.py` and `tools/snapeq.py` come before any
  new script. A new kind of fact goes into one of them.
- **Hand-backs:** at most 15 lines, with the full report in the scratchpad.
  One handover per phase.
- **Limits:** every command under 9 minutes, no background jobs polled with
  sleep, one SHARC replay at a time.
- **Gates:** each phase ends on a measured gate, a commit and a push.
- **Both images:** every differential test runs on dt2-1.16, and on dn2-1.11
  wherever a capture or snapshot exists.

## Architecture target

```
native/
  sharc/        SHARC core: transpiled handlers + runtime (pure, no firmware)
  sharc-jit/    block translator: SHARC blocks -> WebAssembly (or native)
  coldfire/     ColdFire V4e core: ISA_A/B/C, EMAC (pure; the MCF5441x
                has no FPU)
  cf-jit/       ColdFire block translator (shares the backend with sharc-jit)
  periph/       MCF5441x peripherals: INTC, PIT, DTIM, eDMA, SSI, DSPI,
                eSDHC + card, GPIO, UART, FlexBus/DSP FIFO, panel, display
  machine/      board wiring + per-product profile (DT2, DN2): memory map,
                which peripherals, symbols; snapshot import from emu/*.snap
  loader/       .syx extraction (port of dt2/elz.py), +Drive builder (port
                of tools/plusdrive.py)
  live/         desktop audio (cpal), threads, frame queue
  web/          wasm32 bindings: Web Worker, AudioWorklet, file picker
```

The Python emulator (`emu/`, `tools/sharc_core`) stays as the frozen oracle.

## Phases

### P0: housekeeping (small, now)

- Findings verification batch: a second agent checks the [D] sections from
  this round against the image bytes, and the handover is updated.
- A staleness guard: the native SHARC library carries the hash of the
  `sharc_core` sources it was generated from, and refuses to load
  otherwise.
- Em removes the old worktrees.

### P1: gate, the SHARC JIT backend (measure before building)

Question: can blocks translated at run time reach at most 3 ns per
instruction, in the browser as well as on the desktop?
- **A. Floor:** a firmware-free, pre-decoded interpreter over the transpiled
  handlers. Its ns/instruction is also the fallback path's cost.
- **B. WebAssembly per block,** measured under wasmtime (desktop) and in
  Chrome, for the phase-0 hot set and a full drive3 frame. Two ways to
  generate it:
  1. Port the block specialiser (`tools/sharc_rsgen.py`) to Rust, emitting
     WebAssembly with the `wasm-encoder` crate.
  2. Copy-and-patch: compile per-form templates ahead of time (firmware
     free), then stitch them at run time with the decoded fields patched in.
- **C.** Cranelift native code for the desktop, only if B misses on the
  desktop.

Measure: ns/instruction, translation time per block (JIT latency), memory.
The gate picks one backend, or WebAssembly everywhere with native as an
optional desktop speed-up. The output is a decision note with the numbers.

### P2: SHARC JIT

- **Build:** `native/sharc-jit` with the chosen backend. The live path
  switches to it, and the AOT generator is retired; the transpiler stays for
  the handlers.
- **Gate:**
  - lockstep exact on the compute corpus, the phase-0 vectors and the drive3
    frames, plus a Digitone II capture;
  - a patched image runs with no build step;
  - at most 667 us per frame on the desktop, with 0 underruns over 60 s.

### P3: ColdFire core in Rust (runs in parallel with P1 and P2)

1. **Instruction census** (`tools/cf.py`) over both images: which ColdFire
   instructions, EMAC and FPU forms, MMU and cache use, and supervisor
   operations are actually present.
2. **Decoder** generated from an instruction table taken from the public
   manuals, as `tools/sharcspec` is for the SHARC. The semantics are written
   by hand in concrete Rust; they are short for this ISA.
3. **Oracle:** patched Unicorn, per-instruction lockstep on random states and
   on real firmware traces from snapshots of both images.
4. **JIT:** blocks go through the P2 backend.

Gate: lockstep over 100M+ instructions of real traces for both images; an
interpreter speed floor; the JIT at 150M+ instructions/s or more (the device
runs about 132M/s).

### P4: peripherals and machine in Rust (starts with P3)

1. **MMIO recorder:** a tool that records every peripheral access and
   interrupt from Python emulator runs (both images) as replayable traces.
2. **Models:** port each peripheral against its trace, one lane per group:
   timers and INTC; DMA, SSI and DSPI; eSDHC and card; GPIO, UART, panel and
   display.
3. **Drop hooks that only existed for Unicorn or speed.** The MCF5441x has
   no FPU (RM p.106-107), and the P3 census found no FPU instruction in
   either image, so the firmware's float routines are ordinary code: the
   soft-float HLE is only a speed-up, dropped unless profiling needs it. The
   bitmap HLE (speed only) and the idle hook (the core stops at idle itself)
   go too.
4. **Machine profile per product,** DT2 and DN2. The differences are
   recorded in findings.

Gate: from an imported snapshot, the whole machine matches the Python
emulator (snapeq-equivalent state at equal device time) for both images.
DT2 cold-boots to the ready screen.

### P5: integrated desktop emulator

- ColdFire, SHARC and live audio in one native process, with no Python on the
  hot path. The firmware's own frame cadence replaces the forced vector 191
  and the opened frame gate.
- Model the per-track master mix, which removes the DAC injection hand step.
- One UI for desktop and browser: an HTML canvas panel (screen, keys,
  encoders), shown in a local webview on the desktop. The Tk GUI is retired
  after this.

Gate: UI and sequencer at 1.0x with audio, 0 underruns, on both products.

### P6: browser

- A wasm32 build of the cores and machine, with the JIT producing
  WebAssembly modules at run time, running in a Web Worker.
- AudioWorklet and SharedArrayBuffer (cross-origin isolation headers on the
  host).
- A file picker for the `.syx`, with extraction and the +Drive build in the
  page; IndexedDB for images and snapshots.

Gate: the user's own firmware boots in Chrome, Firefox and Safari, and a pad
plays in real time.

### P7: DSP patching

Patch the SHARC image at load time. The JIT makes each change immediate, and
the patched sound is checked against the unpatched one.

## Order and parallel lanes

| Stage | Lanes in parallel |
|---|---|
| 1 | P0 housekeeping; P1 gate (A and B); P3 census and decoder; P4 MMIO recorder |
| 2 | P2 SHARC JIT; P3 interpreter and lockstep; P4 timers/INTC and DMA/SSI/DSPI |
| 3 | P3 JIT; P4 eSDHC/card and the rest; machine profile DT2 and DN2 |
| 4 | P5 integration and UI; loader ports (`elz`, `plusdrive`) |
| 5 | P6 browser; P7 patching |

Keep workflows under about 10 agents; each lane has a single owner per crate.

## Decisions for Em

1. **Merging large changes:** copy the verified worktree files with a byte
   check, instead of a spec re-typed by a coder. Coders stay for small
   edits.
2. **Backend (decided by P1, 2026-09-29):** WebAssembly everywhere, with
   wasmtime on the desktop. The same generated SHARC code runs at 1.9
   ns/instruction native, 2.9-3.3 under wasmtime and 2.9-3.1 under V8. The
   JIT ports the block specialiser to Rust and emits concrete-typed
   WebAssembly, one module per region in a shared table (not
   copy-and-patch). Cranelift-native only if P2 misses 550 us/frame under
   wasmtime.
3. **UI:** one HTML canvas UI for desktop and browser.
4. **Python emulator:** kept as the frozen oracle and not extended further,
   except for the recorders the oracles need.

## Native host checkpoint (2026-09-30)

### Device identity and runner contracts

`devices/digitakt-ii.toml` and `devices/digitone-ii.toml` are the canonical
product identity, release hash, and physical-panel records. The portable
`native/device-profile` crate embeds and validates those documents for native
and wasm runners; the browser parses the same raw TOML. Its optional boot
contract is hash-bound to a MAIN image and names the existing explicit Oracle
MAIN diagnostic contract. It does not claim hardware accuracy or describe
hardware topology. A future topology reference must remain separate and add
only independently evidenced fields. Firmware-specific names and guest
addresses continue to be signature-resolved from the selected image.

`native/loader` now provides a dependency-free, wasm32-compilable SysEx/ELE3
loader. `native/host` is an offline CLI that consumes it: `--syx` decodes
ELE3 section 3 (`MAIN_OS`, destination `0x40000400`) and requires its hash to
match the existing checkpoint profile; `--main` remains available for the
same gate. Section 2 is the bootstrap, not MAIN_OS, and this command does not
cold boot or stage an ELE3 flash image.

The host accepts only recorded Oracle events and `--timer oracle`; Device
timing is explicitly unsupported. `--out` is an absolute, explicit destination
directory for its JSON and WAV; all inputs are validated before it creates that
directory, and an existing file passed as `--out` is rejected. The validated
DT2 1.16 invocation below produced 68 wire TX frames, 204 clean SHARC renders
(three per TX), zero SHARC stops, the expected wire stream, and identical PCM
for `--main` and `--syx`:

```sh
cd native/host
CARGO_TARGET_DIR=/private/tmp/dt2-native-integration-target cargo build --release --offline
DYLD_FALLBACK_LIBRARY_PATH=/path/to/sharc-lib-dir \
  /private/tmp/dt2-native-integration-target/release/dt2-native-host --timer oracle \
  --events "$PWD/../../out/native/integrated-auto-smoke/fast-offers-sixtyeight.json" \
  --profile "$PWD/../../out/native/integrated-auto-smoke/frame-profile.json" \
  --mstate "$PWD/../../out/native/integrated-auto-smoke/ready-v2.mstate" \
  --card "$PWD/../../out/plusdrive/native/dt2.img" --syx "$PWD/../../Digitakt_II_OS1.16.syx" \
  --pack "$PWD/../../out/native/live/state-82cf380735390258438540a4.pack" \
  --sharc-lib /path/to/libsharc_native.dylib \
  --expected-wire "$PWD/../../out/native/integrated-auto-smoke/fast-offers-sixtyeight.dtfr" \
  --out "$PWD/../../out/native/host-syx" --frames 68 --limit 20000000 --render-frames 3
```

This is a bounded checkpoint replay with synthetic queue cadence. It does not
establish autonomous firmware scheduling, sustained GUI execution, a zero-stop
full emulator, or browser integration.

## Current bounded gates (2026-09-30)

The frozen current SHARC core is
`4b25379c9ea67fc5933d9e194ba1d4d13ace66c57c5b5e1919a57fd66f34fd7f`.
The locally generated, exact-provenance artifacts are the native library
`/private/tmp/dt2-sharc-completion/target/release/libsharc_native.dylib`
(SHA-256 `7e1bf5897ed4634df2d02fcee3c6dbf28576a9677a4f70f5ae22ca4b488b402a`)
and initial state pack
`/private/tmp/dt2-sharc-completion/state-pack/state-e39d3187ee0fcd5fb5cfbc2c.pack`
(SHA-256 `52ff0b9bf05a5efaae963a8a83b4afc395b7a37902d739ee1379600ca018fa39`).
Regenerate both after any SHARC semantic change; saved checkpoint-198 and
old-pack ordinal-187 traps are historical diagnostics, not current gates.

| Gate | Result and limit |
|---|---|
| Fresh native replay | 204 inputs: 43,828,934 instructions and final canonical SHA `794efcf929c23633dda872cc82b5c7fbcd52262fef361e739638bbf6a96ab83d`; 256 inputs: 54,247,701 instructions and final SHA `4b840c3c…a891`. Both use the accepted return at `0x1c75d3`; they do not establish full instruction parity. |
| Firmware-free WASM interpreter | Generic transpile output has four semantic files and no `image.rs`. Its Node/V8 run used the pack image at runtime, completed the 204 replay in 4.404s at 43,828,934 instructions with no JIT requests, and reached the same final SHA. The module is 579,724 bytes, SHA-256 `98669a1372d8bcfb325af20d9f2d0d461543f8635aff33e7bfa5e082c7194742`. This is neither browser nor realtime execution. |
| Native host checkpoint | Fresh `--main` and `--syx` runs deliver 68 TX frames, 204 clean renders, zero stops and zero DMA failures. Their exact wire SHA is `e21cb015102bd6c1222b7c733238ddf7402a725ee7e63b0fb8313d00fd1f8e8b`, PCM SHA `b7f12d308007e0d4b746cae09136d758cb808dfdbd666d0f1e098befaec73b97`, and WAV SHA `4771ca619239a277cffcb8008095d6c087321607d83cc5e58fad3f33494a0f9e`; reports include `host-main.wav` and `host-syx.wav`. The host returns failure after writing reports for stops, DMA failure, incomplete work, or a wire mismatch. `--rendered-input-log` is optional and accepts renders of at most 4096 bytes. |

The runner recipe is `fresh_call(dma_callback, -1)`, set R8, step 64,
then `fresh_call(handler, -1)` and step 500,000. An earlier scratch run set
R8 after the DMA step and therefore observed idle handlers; that was corrected
as a runner-ordering mistake, not a SHARC core defect.

The corrected Type15b evidence cites the *SHARC+ Core Programming Reference*
(SC58x/2158x Rev. 1.5), printed pp. 16-8--16-12 (extracted pp. 0391--0395);
Type3c is extracted pp. 0323--0324. In the historical ordinal-187 probe,
`0xb829eb` is the software PC and `0x3106fc64` is the attempted address.

Rust GPIO has an optional hook; Oracle INTC uses zero masks with Device reset
preserved, and `+14` applies to INTFRCL rather than IMRL. Board capture owns
pending/outbound TX through `take`; both TX35 and DSPI use SERQ-all. The
full-machine gate reports 83 passing and 13 ignored tests plus its wasm check.

`native/boot` promotes the bounded Oracle cold-boot diagnostic. It requires
an embedded-registry SYX/MAIN identity, resolves its signatures from the
selected image, and writes reports only to an explicit empty absolute output
directory. Its compatibility services (zero pages, forced status reads, flash
HLE, timers, idle interrupts, and TX35) require `--diagnostic-services`:

```sh
cargo run --manifest-path native/boot/Cargo.toml --offline -- \
  --syx "$PWD/Digitakt_II_OS1.16.syx" --out /private/tmp/dt2-boot \
  --mode oracle-diagnostic --limit 1000000 --stop-at limit --diagnostic-services
```

This is a bounded diagnostic observation with completed firmware panel-frame
capture, not a hardware model, DSP boot, panel input implementation, or
resumable checkpoint. Full Device/autonomous DT2/DN2 boot, UART RX/panel
input wiring, audio, realtime JIT, and browser runtime remain unfinished.

For a reproducible invocation, `mise run boot -- FIRMWARE.syx` builds the
native diagnostic offline, enables its explicit Oracle diagnostic bundle, and
writes a fresh report directory under `out/native/boot/`. Pass `--out DIR`,
`--card-image FILE`, `--limit N`, or `--stop-at ready|limit` to set runner
inputs; relative paths are resolved from the caller's directory. A card image
must be a readable regular file whose exact 512-byte-sector capacity is one of
the two supported eMMC identities; it is mounted read-only beneath the guest's
sparse write overlay and its streaming SHA-256 is recorded in the report.
With `--diagnostic-services`, the runner derives the eSDHC bring-up completion
words from the loaded MAIN image and records them in `resolved-profile.json`;
this is an Oracle synchronous-completion diagnostic, not a Device ISR model.

`mise run plusdrive -- build SAMPLES -o NEW_IMAGE` creates a sparse sample-only
image. Add `--syx DT2_1.16.syx` to seed the verified DT2 1.16 default project;
`mise run plusdrive -- ls IMAGE` lists root entries. Reusable bounded commands
are:

```sh
mise run plusdrive -- build SAMPLES -o out/plusdrive/dt2.img --syx DT2_1.16.syx
mise run plusdrive -- ls out/plusdrive/dt2.img
mise run boot -- DT2_1.16.syx --card-image out/plusdrive/dt2.img --limit 1000000000 --stop-at ready
```

For DN2 main-panel observation, omit `--syx` when building the sample-only
card, then use the same boot form with the DN2 SYX and that card. These
commands remain Oracle diagnostics, not claims of Device interrupts, audio,
SHARC completion, or browser runtime. DT2 `VERIFIED_READY` covers its
image-resolved filesystem check and main panel; DN2 `VERIFIED_READY` covers
the main panel only. Full hardware, audio, input, GUI, and WASM bridge work
remain pending.
