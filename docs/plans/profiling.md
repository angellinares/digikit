# Profiling plan

Date: 2026-11-10. Base: `main` at 56fbc119. Targets: DT2 OS 1.17, DN2 OS 1.12.

Status: **draft for Em's review. Nothing is built.**

This plan replaces `~/src/digi/SHARC-PROFILING-PLAN.md` (2026-11-05, outside the repo). It keeps that plan's still-valid content and drops its
checkout and lane state. It works with [runtime-and-services.md](runtime-and-services.md) (where device hooks may live),
[debug-mode-and-probes.md](debug-mode-and-probes.md) (log sink, `digikit/debug`, probes) and
[workbench-device-log.md](workbench-device-log.md) (the Log pane). The order of its items, together with the runtime-and-services items,
lives in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md).

## 1. Goal

1. Profile modules (machines, FX, LFOs, filters, tools) and the runtime, on the emulator and on the real device.
2. Make SHARC emulation faster on the host, with guest output unchanged.
3. Name what we measure: every address in a profile, log or crash record shows as a function name where we have one.

The work has two tracks with separate first slices:

| Track | What | Depends on |
| --- | --- | --- |
| A | Emulator host speed: attribute host CPU to firmware functions and pick optimisations | Nothing. Can run any time in a lane |
| B | Module and runtime profiling: address map, shared library, `wb:cli`, workbench panes, emulator cost, device timing | Device parts wait for runtime-and-services sink slots and dsp-link windows |

Native and browser speed are separate questions. A native gain proves no browser gain.

## 2. What exists today

- **sharcdb** (`out/sharcdb/<image>.sqlite`) has blocks, basic blocks and successors, loops, call graph, `regdef`/`reguse`, `mem_access`,
  reachability and function hashes. **cfdb** (`out/cfdb/<image>-emac.sqlite`) has functions, edges, data refs, strings, symbols, RTTI and
  `ported` names, but no basic blocks and no coverage column. `tools/coldfire/fw.py` queries both.
- **SHARC run-time data:** `SHARC_PROFILE_PREFIX` coverage, entry and transition maps; `--diag hist`; `fastcov` (names functions from sharcdb);
  `--profile-regions`; `SHARC_FAST_TIME`, `SHARC_TIER_TIME`, `pcdump`, `wwatch`, `SHARC_UNINIT_TRAP` ([INSTRUMENTATION.md](../INSTRUMENTATION.md)).
- **ColdFire run-time data:** write watch (stores only), `--span`, `regions_probe --pc`, the `cf-profile` feature (a PC histogram sampled
  every 64 instructions). No exact coverage, no breakpoints, no symbol lookup in the daemon.
- **Names:** `fastcov` and `perf_calls.py` name stock SHARC PCs only; module PCs print as `?`. `snapread.py` names ColdFire addresses from the
  hand-made 1.16 `cf_names` list. Many hot SHARC functions have no name ([finding 18](../findings/18-dsp-performance.md)).
- **Module cost:** authors declare `max_cycles_*` by hand. `module:check` checks a worst-block limit for effects only (`pk_conform.fx_budget`).
  HEALTH shows the whole DSP block time, not per module.
- **Module DSP code is never AOT.** It loads into L2 at run time and runs in the fast tier or the interpreter.
- **Device:** the debug module answers host polls (LOG, PEEK, LOGCFG); a 512-event ring, at most 27 events per read. The probe module owns
  `dl_set_tick_cb`. The crash record holds PC, registers, 32 stack words and the module index. The device transport (`dklog`, `dkupload`)
  has not run on hardware [O].

## 3. Address map

Host-side only. The daemon and the device never read the firmware databases.

### 3.1 Layers

| Layer | Source | Availability |
| --- | --- | --- |
| Our code | Runtime build symbols (`patchkit cf-link --json` emits symbols and `text_ranges`, `packages/patchkit/src/build_cli.rs:212-216`); module `.dko` symbols kept as a sidecar in `out/` | Always. We build it, so the site and browser users can have it |
| Anchors | `devices/ports/*.toml` (committed): name, address, length, hash per image | Always. Coarse firmware names for hook points and key stock functions |
| Firmware databases | cfdb and sharcdb | Developer machines only. Never shipped, never committed |

A lookup uses every layer present and falls back to the raw address. A map is keyed by firmware image hash, runtime build hash and module
hashes. For a return address, look up the address minus 2: a call to a function that does not return is often its last instruction
([sdk-api.md](sdk-api.md), "Later", the open item on host-side names for panic records; item 1 in section 13 covers it).

### 3.2 Load addresses

Module code is relocated at load time, so the map needs each module's load address per run. `MOD_LOAD` carries selection index, result and
the file name; `MOD_LIST` carries index, slots mask and name ([finding 46](../findings/46-digikit-debug.md) table rows 5 and 21). Neither
carries a load address. Options: add the ColdFire and DSP load addresses to a kind-free event, or read the runtime's module table with PEEK.
In the emulator, the daemon can read it directly (`mem.cf.peek`, `dsp.peek`).

### 3.3 sharcdb tables for our own DSP code

Run the sharcdb builder on each module's DSP section, with the `.dko` symbols as names. A module then gets blocks, loops, register use, memory
accesses and a call graph from the same tables we use for the firmware. Uses: static loop cost, the DSP rule check
([sdk.md](sdk.md) section 9), and naming in profiles. First test: does the builder accept a relocatable L2 section? [O]

### 3.4 Export

A Python tool under `tools/` exports cfdb, sharcdb and anchors for one image into a compact map file under `out/`. Python stays for this
export, because it reads the reverse-engineering databases. Everything that reads the map is Rust or TypeScript.

## 4. Shared library, CLI and workbench

One Rust crate holds the profile model and the address-map lookup, with TypeScript bindings for the desktop workbench. The CLI and the
workbench call the same code (`mise run wb:cli` runs workbench actions through the workbench's own code).

- **CLI:** new `wb:cli` subcommands, for example `profile record`, `profile report`, `addrmap lookup`. Output is text or JSON (`--format json`).
  JSON is the artifact format; text is a view of it.
- **Artifact:** one schema for emulator and device runs (section 9). The pane, the CLI and replay readers read the same artifact and reach the
  same numbers.
- **Workbench consumers:**
  - The Log pane ([workbench-device-log.md](workbench-device-log.md)) shows names for addresses in log events and crash records.
  - New panes: DSP and ColdFire utilisation per module or owner over time, per block, and per function in the emulator. Interval graphs,
    markers, record and stop, export.
- The workbench never runs Python.

## 5. Track A: emulator host speed

Goal: explain host CPU of the SHARC thread and find headroom with guest output unchanged. The 2026-11-05 figure (DN2 DSP CPU per audio
second about 1.05) is history. Re-measure it on DT2 1.17 and DN2 1.12 before using it.

First slice: put sharcdb names on host profiles. Generated code shows up in host samples as `r_<PC>` (for example `r_1C6EA9`). Joining these to
sharcdb functions gives host time per firmware function. That says where AOT and fast-tier work pays. It is a static join.

Then:

1. An optimised symbol build with source, UUID and PGO provenance recorded. Debug information can change layout and PGO compatibility; no
   automatic PGO training.
2. One representative workbench case: warmup, idle, verified sounding notes, page changes, a realistic machine and FX load. Keep the exact
   input sequence and phase markers. Capture clean timing separately from sampling and counters.
3. Rank generated execution costs, including shared inlined helpers. Pick one cross-cutting mechanism with a measured or bounded
   removable-cost estimate. Symbol share and pipeline-model fractions are not a savings ceiling.

Notes kept from the 2026-11-05 work (historical, recheck before use):

- Raw leaf PCs often have address modulo 4 equal to 1. The convention is undocumented; do not subtract 1 and claim exact attribution.
- Physical symbol frames lack inline ancestry. Weights from a source-attribution build do not transfer to a PGO build.
- Two fallback roots reduced interpreted work by about 39%, but the paired host CPU gain was 1.9961%, which failed the 2% criterion.
- Loop undo-log sequences recur in hot code; their removable cost is unknown. Static instruction counts are not dynamic cost.
- Frozen reference: `out/native/runs/20261104-sharc-profile/current-pgo/` (binary `frozen/dn2-coupled`, UUID
  `BA287112-4EDD-37A6-B274-9BF47E0F2F1C`).

## 6. Track B: emulator profiling

No clock reads in the emulator. Counter reads can change execution tiers: `tools/sharc_dev/sharc_rsgen.py` protects EMUCLK, EMUCLK2 and
TCOUNT in `UNSAFE_READ_NAMES`. Per-function detail comes from PC counts plus the address map instead.

1. **Names everywhere.** `fastcov`, `--diag hist`, `--profile-regions`, `pcdump`, `wwatch`, `SHARC_UNINIT_TRAP`, `--span`, `cf-profile` output,
   crash records and SHARC halts go through the address map.
2. **Per-module cost.** For one module: instructions per block, split by function and loop, from PC counts. Runs are deterministic, so
   counts repeat. These are instruction counts, not hardware cycles; section 7.3 calibrates them.
3. **Machine budget check in `module:check`.** Run the machine at 16 voices on both devices and compare its worst block with the device
   headroom (about 450k instructions on DT2, 330k on DN2, `digikit-module` skill `references/devices.md`). Effects already have
   `fx_budget`. The measured number feeds the packer-derived cycle need ([sdk.md](sdk.md) section 9).
4. **Fast-tier refusals per module.** List the module instructions the fast tier refuses, which keep it in the interpreter. This matters
   for emulator and browser speed, not for hardware. Input for an SDK warning and for fast-tier lowering work.

## 7. Track B: device profiling

### 7.1 Where the timing lives

- **`DK_SPAN(owner)`** in the SDK, for ColdFire and SHARC: read the cycle counter before and after, emit `(owner, cycles)`. When nothing
  listens it costs one flag test.
- Each kind's dispatcher uses it once, around its per-row call. The dispatcher belongs to the service that owns the kind: `machines`
  around each voice render, `fx` around each process call, a future LFO service around each shape. Content modules never contain it.
  A new kind gets timing by adding one `DK_SPAN` to its own service; the runtime does not change.
- **Sink slot.** Spans go to a runtime sink slot of the same kind as the log sink (`dkrt_log_sink_set`, debug-mode-and-probes.md): one
  pointer test when no sink is set. The runtime names no kind and no service; owners are the opaque ids it already has
  (`owner_of_row`). The slot mechanism is runtime-and-services section 5.11; the dispatcher owner is its section 6.1.
- Do not take the probe's `dl_set_tick_cb` slot or the debug sink.

### 7.2 `profile.dks`

A loadable service, loaded only when a developer asks. It fills the span slot, aggregates per owner per block in a fixed table (at most
`SEL_MAX` plus provider rows), and answers host polls through the debug transport. Not installed means nothing is paid except the flag tests.

DSP side: the SHARC dispatchers write `(owner, cycles)` into a stats window registered through dsp-link (window registration is the dsp-link
target in runtime-and-services section 6). `profile.dks` reads the window from the ColdFire side. Whether `dkrt_peek_range` or a new command is
the host transport is open [O].

The debug ring (512 events, at most 27 per read, overwrites old events) is for markers, not for high-rate traces. Aggregate on the device.

### 7.3 Calibration

Run the same scenario on the emulator and the device, and compare emulator instructions with device cycles per module. The ratio is the stall
factor the emulator does not model. With it, the emulator budget check (section 6) can be trusted before hardware. Calibrate also: read-pair
overhead, IRQ effects, counter wrap and observer cost, with controlled cases. Emulator instructions and device cycles stay separate
measures in every report.

### 7.4 Not on the device

- **No bracketing of stock firmware functions.** That needs hook tables; only a `.dks` may hold them, and two services claiming one site
  are refused (runtime-and-services 5.2). Measure stock functions in the emulator (PC counts, `--span`). On the device, keep the whole-block
  number that HEALTH reports.
- **ColdFire PC sampling** comes later, after the device transport works: a timer interrupt counts the stacked PC into a histogram on the
  device, and the host pulls it and names it with the address map. Not through the debug ring.

## 8. Clocks

Keep clocks separate in recordings and reports.

| Quantity | Meaning | Limit |
| --- | --- | --- |
| Native worker CPU seconds | Host thread CPU | Independent of guest EMUCLK |
| Audio seconds / DSP block clock | Block progression, nominal 1500 Hz | Verify source and reset |
| Emulator EMUCLK | Instruction-clock base plus instruction count | Includes idle replay; not hardware cycles |
| Emulator core timer | Separate instruction-based model | Not derived from EMUCLK |
| Hardware EMUCLK/EMUCLK2 | Low and high core-cycle counter | Includes IDLE; pauses under a hardware debugger; needs calibration |
| ColdFire idle/load estimates | Relative idle or instruction/CPI estimates | Not absolute utilisation, not host CPU |

Public source: SHARC+ Core Programming Reference, Rev 1.5 (`sc58x-2158x-prm.pdf` in [SOURCES.md](../sharc/SOURCES.md)), printed pages
29-18 and 29-19.
Low EMUCLK is 32 bits, EMUCLK2 takes the carry, user access is read-only.

Health reads, checked against database and image bytes on 2026-11-05 (recheck before [V]):

| Firmware | Start high / low | End high / low | Subtraction | Store |
| --- | --- | --- | --- | --- |
| DT2 1.17 | `1c74e9 / 1c74eb` | `1c75a9 / 1c75ab` | `1c75ae` | `1c75c4` |
| DN2 1.12 | `1c9d87 / 1c9d89` | `1c9e47 / 1c9e49` | `1c9e4c` | `1c9e62` |

The result is end-low minus start-low modulo 2^32. High words are read but the result is not a 64-bit delta.

**Clock audit (first step of track B device work):** list every EMUCLK/EMUCLK2 consumer with units, reachability and confidence, using
`fw.py` and sharcdb `mem_access`/`reguse`, on DT2 1.17 and DN2 1.12. Find out whether any clock read affects scheduling, timeouts or control,
or only reporting. A database sweep can mistake data bytes for reads; `15a` forms with EMUCLK as destination are writes. If a functional
dependency is found, isolate its fix and proof first. This is not a cycle-accurate emulator project.

## 9. Modes and metric contract

Modes:

- **OFF:** producers disabled. Distinguish "module absent" from "loaded but off".
- **OVERVIEW:** existing counters and low-rate aggregation.
- **SELECTED_MODULE_TIMING:** chosen modules or routes; aggregate all calls or time 1 in N, with sample counts.
- **BOUNDED_DIAGNOSTIC:** selected regions, extra events or the host sampler; stops by itself.

Fast producers do no formatting, allocation or blocking I/O in audio or interrupt paths. Calibrate OFF, loaded-OFF, OVERVIEW and selected
modes on the emulator and the device. Set an overhead budget after a pilot, not before.

Every metric has: name and version, provider, source clock and unit, cumulative or interval, start, end and reset epochs, numerator and
denominator, sampling and coverage, validity. A metric that a target cannot provide is N/A with a reason, never zero (host CPU on the
device, for example).

Every artifact has: schema version, run id, firmware, runtime and module hashes, workload and readbacks, clock configuration, log levels,
binary hash, source revision, PGO treatment, target, phase markers, loss and overhead. Keep raw counters. Never add overlapping inclusive
costs, or host wall waits to thread CPU, as if they were separate work.

Known limits: HEALTH rejects block deltas of 0 or at least 4,000,000; report the invalid count. SDK `max_cycles` values are budgets, not
measurements. Sink underrun events are not DSP missed deadlines.

## 10. Proof discipline for optimisations

- Choose one bounded semantic experiment after attribution and Em's OK, before full-core builds.
- A loop candidate covers non-final and final iterations, nested and fallback paths, pending operations, bank state, IRQ, timer and budget
  boundaries, trap priority, rollback and journal overflow.
- Compare against the unchanged baseline at matched retired boundaries, with canonical state and memory. A route without the generated core
  proves nothing about generated regions it never runs.
- Existing tools: `live_capture.rs` (canonical AOT pre/post state, one PC, at most 16 snapshots), `region_bench.rs` fast_compare (state at
  matched boundaries). `sharc_regionbench.py` source checking is not state equivalence.
- Finish with one predeclared paired `coupled_ab.py` run: exact PCM, canonical state, instruction count, audio duration, same-label
  repeatability. No extra runs after a failed criterion. PCM pin changes need Em.
- Generated semantics live in `tools/sharc_dev/sharc_core/`. A fingerprint change follows AGENTS.md (back up, regenerate both devices,
  `mise run gen:core`).

## 11. Rejected or parked

- Generic fallback recovery (`SHARC_FAST_RECOVER`): parked, unproven.
- Broad mask rewrites, the generic concrete-V rewrite, small guard, alignment and word-band experiments, repeated root lists:
  rejected without new evidence (`docs/design/aot-mask-free.md` 6.4 and 8).
- Repeated PGO on/off comparisons or automatic retraining as the project. PGO is known to help; keep its treatment per variant.
- A cycle-accurate emulator.
- Byte or instruction tracing as a performance baseline.

## 12. Later: firmware tooling

Build each item when a concrete need appears, not before.

| Item | Payoff |
| --- | --- |
| Exact ColdFire coverage (cargo feature, one bit per instruction start) joined to cfdb | Shows which firmware code a test runs; base for the next three items |
| Anchor coverage per test and device | An anchor that resolves but no test runs is unproven on that device |
| Feature location by diffing two deterministic runs (baseline, baseline plus one action), with write watch on a known global | Finds the code for a new hook point (LFO, p-locks, MIDI) quickly; candidates go to `atlas catalog` |
| Run-time indirect call edges and data refs, written to cfdb with provenance `emu:<scenario>` | Fewer missing callers; struct field accesses through pointers that static analysis missed |
| Unmodelled peripheral accesses and `SHARC_UNINIT_TRAP` hits, named | Emulator gaps reported by function, not raw address |
| SHARC static against dynamic: AOT share of reachable code, instruction forms never run by tests, unlowered fast-tier forms in reachable code | Finds interpreter cliffs and untested semantics before a user hits them |
| PGO training coverage: which generated functions the training touched | Tells when to add a training scenario |
| Firmware version porting by aligning call sequences of one scenario on two images | Pairs functions that hash matching cannot; less work per OS update |
| Native kernels for recognised hot DSP functions (biquad, FIR) | Only after a ceiling from `--profile-regions`; must be bit-exact against golden hashes |

## 13. Items

Item numbers are ids, not an order. The order lives in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md); this table keeps each item's dependencies.

| # | Item | Track | Agent time | Depends on |
| --- | --- | --- | --- | --- |
| 1 | Address map v1, emulator only: `.dko` symbols kept, map export, names in emulator tools and crash records | B | 1-2 days | Nothing. Runs in parallel with runtime-and-services item 4 |
| 2 | Shared Rust library and `wb:cli` profile and addrmap commands with JSON output | B | 1-2 days | 1 |
| 3 | sharcdb tables for module DSP sections | B | 1 day | 1; the [O] in 3.3 |
| 4 | Per-module cost, machine budget check, fast-tier refusals per module | B | 1-2 days | 1, 3 |
| 5 | Host profiles named from sharcdb, first ranking | A | 1-2 days | Nothing; in a lane |
| 6 | Workbench: names in the Log pane, first utilisation pane | B | 2-3 days | 2 |
| 7 | Clock audit | B | 1 day | Nothing (static) |
| 8 | `DK_SPAN`, sink slot, `profile.dks`, DSP stats window | B | 3-4 days | runtime-and-services sink slots, dsp-link windows, the owner of DSP dispatch |
| 9 | Device calibration | B | 1 day | 8; the device transport proven on hardware |
| 10 | ColdFire PC sampling on the device | B | 2 days | 9 |

Before each item over a day: state payoff and confidence, and stop when they are weak.

## 14. Open items

- [O] Does the sharcdb builder accept a relocatable module DSP section?
- [O] Load addresses: a new event field or a PEEK of the runtime's module table?
- [O] Host transport for the DSP stats window: `dkrt_peek_range` or a new command?
- [O] The device transport (`dklog`, `dkupload`) has not run on hardware.
- [O] Do any EMUCLK reads affect control, or only reporting (clock audit)?
