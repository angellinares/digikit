# Plan: a firmware-agnostic native emulator (desktop and browser)

Status 2026-09-29, base commit `95d92de` on `work/sharc-emulator`.

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
| ColdFire core (P3) | All 101 used forms; lockstep vs patched Unicorn clean on fuzz and EMAC. **47.7M instr/s** on real code (target at least 62M useful). **Open divergence at pc 0x401768a6** in the boot280M snap lockstep; it predates the speed work and is uncharacterised. | `native/coldfire`; `tools/cf_lockstep.py {snap,fuzz,emac}` |
| Peripherals (P4) | Timers, INTC, eDMA, SSI0, DSPI, DSP FIFO replay the traces with 0 register mismatches. The two boot traces keep about 20 vector-208 timing differences each. Missing: eSDHC + card, GPIO, UART, panel, display. | `native/periph` (`mmio-replay <trace> [--limit N] [--verbose]`); traces in `out/mmio-trace/` from `tools/mmio_record.py` |
| Machine (P5) | Designed, not built. | `docs/design/p5-machine.md` (ten steps, oracle vs device mode) |
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

## Where we are

| Part | State |
|---|---|
| SHARC semantics | `tools/sharc_core` (Python), the single source; exact against real frames. |
| Native SHARC | `native/sharc`: transpiled handlers plus runtime, and a per-block ahead-of-time (AOT) generator. 1.9 ns/instruction, 411 us per frame (budget 667 us). The generated blocks embed the firmware, so they are built locally under `out/`, and each image needs a rebuild (about 80 s). |
| Live audio | `native/live` (cpal); `emu/gui.py --live-audio` plays TRIGs live. |
| ColdFire | Unicorn with 5 patches plus about 30 Python peripheral models and hooks. 0.22x real time with the audio clock running; about 0.4x is the ceiling of this design. |
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
