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
- **Module cost:** derived since S5a. `module:check` writes the worst steady block (a machine, per voice) or process call (an effect) into the header as `DSP_COST_VOICE` and `DSP_COST_FX`; the `max_cycles_*` descriptor words are gone.
  HEALTH shows the whole DSP block time, not per module.
- **Module DSP code is never AOT.** It loads into L2 at run time and runs in the fast tier or the interpreter.
- **Device:** the debug module answers host polls (LOG, PEEK, LOGCFG); a 512-event ring, at most 27 events per read. The probe module owns
  `dl_set_tick_cb`. The crash record holds PC, registers, 32 stack words and the module index. The device transport (`dklog`, `dkupload`)
  has not run on hardware [O].

## 3. Address map

Host-side only. The daemon and the device never read the firmware databases.

### 3.1 Layers

Built (item 1, emulator only): format, lookup and consumers are in [ADDRMAP.md](../ADDRMAP.md); this section keeps the design.

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

Module analysis is the Rust crate `packages/dksharc` (README there). The workbench uses it through wasm and never runs Python, so the
module DSP analysis does not go through the Python sharcdb builder. The crate reads a module's DKDS section and its DSP `.dko` and
decodes the text with the decoder of `sharc_rt`. It gives functions (named from the `.dko`), basic blocks with delay slots, DO loops (literal
or register count, nesting), calls inside the module, import calls resolved through relocations (`import:<symbol>`) and the memory
operands of each instruction at form level. A decode that is not confident is reported and never guessed. On the pluck module for the
Digitone II it gives 306 instructions, 3 functions, 4 DO loops (one literal count of 32, three register counts) and 4 import calls; the Python
module mode of sharcdb, an oracle only and not merged, gave the same numbers. Slice 2 (done) adds register def/use per instruction and per function (registers written on any path to a return, MODE1 net effect), from a
table generated from `tools/sharc_dev/sharc_core` (`tools/sharc_dev/sharc_defuse_gen.py`), so the instruction semantics stay in one place. Uses: static loop cost, the DSP rule check
([sdk.md](sdk.md) section 9), and naming in profiles.

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
  - The Log pane ([workbench-device-log.md](workbench-device-log.md)) shows names for addresses in log events and crash records (P6, done: ADDRMAP.md "Workbench"; the same dk-session and debug-module PEEK path serves the emulator and
    hardware, and hardware proof is a later explicit step). The Memory panel lists what each module takes of each resource (P6, done).
  - The Profile pane (P8b, desktop; `apps/desktop/ui/src/ProfilePanel.tsx`, logic in `packages/workbench-core/src/profile/`): ColdFire share per module, runtime phase and poll over a 60 s graph with markers, record and stop, export of `dk.profile/1`; on the emulator also the modules' DSP per block (`sharc.profile`) and `cf.profile` with the same Start. The device's spans come over the same dk-session path as the emulator's (the debug module). Use is in [INSTRUMENTATION.md](../INSTRUMENTATION.md#profiles) ("Profile pane"). Per-function ColdFire views on a device wait for P10, per-module DSP on a device for the DSP stats window.
- The workbench never runs Python.

P2 design:

- **Crate:** `packages/dkprof` builds on `dkaddrmap` and `dksharc`. It turns raw profiles into named per-module and per-function self aggregates.
- **Wasm:** one `dkprof.wasm`, built by `tools/nx/wasm.mjs`, with C-ABI calls that take and return JSON: `addr_lookup`, `load_table`, `profile_report`
  and `module_analyse` (`dksharc`). A TypeScript loader lives in `packages/dk-session`.
- **Artifact:** schema `dk.profile/1` (section 9): `run`, `clock`, `raw` (`{cpu, addr, count}`), `metrics` (a value, or N/A with a reason), phase markers, and loss and
  overhead.
- **`wb:cli`:** a global `--format json|text`, and the commands `addrmap lookup`, `module analyse`, `profile record` (emulator: `cf.profile` through control-client) and
  `profile report`. All go through `dkprof.wasm`; `wb` starts no child process for them.
- **Python:** `tools/devtools/cfprof.py` is deleted (done): on one paused emulator, `wb profile record --since-boot` and `cfprof.py` gave equal path counters and equal function, idle and exit rows.
- **Not in P2:** the SHARC profile source (a new daemon call; done in P4: `sharc.profile`), the device source and the Profile pane are P8; P6 (Log names, Memory per-module rows) is done.
- **patchkit:** it uses the dksharc DKDS and `.dko` readers (done; packed bytes unchanged).

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
2. **Per-module cost (P4, done).** `sharc.profile` ([CONTROL.md](../CONTROL.md)) returns executed instructions per software PC as `dk.profile/1` raw counters; `wb profile record --cpu sharc` writes them and
   `wb profile report` names them by module and function (address map, P3b load table) and by DO loop (`dksharc`), with instructions per DSP block. The source is the dispatch profile of the fast
   and block tiers (an entry PC carries the whole region that starts there) plus the interpreter's per-PC counts, behind the `sharc-profile` feature (default build unchanged). Runs are
   deterministic, so counts repeat. These are instruction counts, not hardware cycles; section 7.3 calibrates them.
3. **Machine budget check (P4, done).** The rule is Rust: `dkprof::budget`, run by `patchkit budget` and read by `module:check` (`pk_conform.budget`, `fx_budget`; the numbers equal the former
   Python's). Units are instructions per block. Per device the headroom is the block at 85 percent (projected with the "high" factor of `devices/cycle-factors.json`) minus the stock baseline
   (`stock_busy`); today 194,244 instructions on DT2 and 92,796 on DN2 (`patchkit budget`, from `devices/*.toml` and `devices/cycle-factors.json`). The machine check is
   `voice-block <= headroom / voices` for the steady worst block and `note-on <= headroom` apart; `dkrt_fit::check_selection` (integers, `no_std`; `dkprof` re-exports it) is the rule the broker applies since S5a: the planner, the menu and `patchkit fit` use it with the cost that `module:check` writes into the header ([sdk.md](sdk.md) section 9).
4. **Fast-tier refusals per module: moved to P5** (emulator speed track, parked). It lists the module instructions the fast tier refuses, which keep it in the interpreter. This matters
   for emulator and browser speed, not for hardware; it is input for an SDK warning and for fast-tier lowering work.

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

### 7.2 Spans in the debug module

The runtime emits spans from its ColdFire call sites (module ticks, service phases, polls) to a sink owned by the debug module. The clock is the stock
DTIM0 counter, read only. The debug module aggregates on the device per owner: calls, a 64-bit sum and the maximum. The host reads the table with
PROFILE_STATE (0x1e: get, start, stop, reset) and PROFILE_READ (0x1f). Not loaded means nothing is paid except the flag tests. The result is the
additive `spans` field of `dk.profile/1`, and `wb profile spans` runs the capture.

DSP side: the SHARC dispatchers write `(owner, cycles)` into a stats window registered through dsp-link (window registration is the dsp-link
target in runtime-and-services section 6). The debug module reads the window from the ColdFire side. The DSP transport is open [O].

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

Known limits: HEALTH rejects block deltas of 0 or at least 4,000,000; report the invalid count. The cost in a header is the worst case of the conformance cases, not a
measurement of a real song. Sink underrun events are not DSP missed deadlines.

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
| 1 | Address map v1, emulator only: `.dko` symbols kept, map export, names in emulator tools and crash records. **Done** (docs/ADDRMAP.md) | B | 1-2 days | Nothing. Runs in parallel with runtime-and-services item 4 |
| 2 | Shared Rust library and `wb:cli` profile and addrmap commands with JSON output | B | 1-2 days | 1 |
| 3 | Module DSP analysis: the Rust crate `dksharc` (slices 1 and 2 done: register def/use) | B | 1 day | 1 |
| 4 | Per-module cost, machine budget check (fast-tier refusals moved to item 5's track). **Done** | B | 1-2 days | 1, 3 |
| 5 | Host profiles named from sharcdb, first ranking | A | 1-2 days | Nothing; in a lane |
| 6 | Workbench: names in the Log pane (emulator and device, one dk-session/debug-module PEEK path; hardware proof later), per-module rows in the Memory panel. **Done** (docs/ADDRMAP.md "Workbench"); the Profile pane moved to item 8 | B | 2-3 days | 2 |
| 7 | Clock audit | B | 1 day | Nothing (static) |
| 8 | Spans from the runtime's ColdFire call sites to a sink owned by the debug module; clock = stock DTIM0 counter (read only); aggregation on the device per owner (calls, 64-bit sum, max); PROFILE_STATE/PROFILE_READ (0x1e, 0x1f); additive `spans` in dk.profile/1; `wb profile spans`; DSP stats window, Profile pane (emulator `cf.profile` and device, over the same dk-session path as the Log names) | B | 3-4 days | runtime-and-services sink slots, dsp-link windows, the owner of DSP dispatch |
| 9 | Device calibration (`wb profile calibrate`; design in 13.2) | B | 1-2 days | 8 (P8d); the device transport proven on hardware, with Em's go |
| 10 | ColdFire PC sampling on the device | B | 2 days | 9 |

Before each item over a day: state payoff and confidence, and stop when they are weak.

### 13.1 Item 8 remaining steps: P8c, P8d, P8e

Items marked [inferred] or [open] were not checked.

**What exists.**

- ColdFire spans: `dlog::spanned`, `span_t0`, `span_end` (`modules/core/runtime/src/rs/src/dlog.rs:281-339`). Producers: module ticks (`entry.rs:116-121`), service phases as `0x100|p` (`tick.rs:15`), polls as `0x200|i` (`tick.rs:25`).
- Debug-module span table: one row per module, per phase (3) and per poll (4) (`modules/tools/debug/src/c/debug.c:71-74, 655-678`); PROFILE_STATE and PROFILE_READ at `debug.c:706-766`. The host derives the layout as `rows - 7` (`packages/dk-session/src/dkupload-core.ts:1319-1341`). `dkprof` accepts only the kinds module, phase and poll (`packages/dkprof/src/profile.rs:176`).
- SHARC contract defined, not implemented: `DK_SPAN_BEGIN/END`, `_dk_stats_ptr`, a window of 4 + 16x4 words (`sdk/dsp/contract/dk_trace.inc:19-70`, `sdk/include/dk/core/trace.h:38-46`). `_dk_span_add` and the window exist nowhere. Window 3 is reserved and refused on both sides (`abi/src/link.rs:12,19`; `dsp-link.inc:28,181`).
- R10 still keeps SHARC dispatch in the adapters: `_fx_mark` stage 4/5 (`modules/core/adapters/fx/src/dsp/fx.s:290-307`), `_dv_mark` (`dsp-voice-dn2.s:294-302`; DT2 the same [inferred]).

**P8c, ColdFire dispatcher spans (about 1 day).**

- Dispatch points: spans sit at the interface-B boundary, one instrumentation point for all services and modules, the same for linked crates and `.dks` files. (1) Service calls: where the core calls a service through its `ServiceRow` in the service table dispatch (`ServiceTable`), owner `0x100 | svc<<4 | phase`, one span per service and phase; hook calls through the `ServiceRow` (page key and page turn) use owner `0x300|hook`. (2) Module entry calls: where the core calls a module's `dk_module` entry. (3) Module ticks and polls are unchanged.
- Service x phase spans replace the phase spans. The host sums the rows of a phase to get the phase total. Overlapping spans may not be added (section 9).
- Record format unchanged: `(owner, cycles)` on the span channel.
- Debug module: `span_row` gains the hook class and the service x phase rows. PROFILE_STATE reports its class counts (modules, service-phases, hooks, polls); today the host hard-codes `rows - 7`, and a tail after the poll slots would parse as more polls. This is a format change: the debug module and dk-session change in one PR.
- Cost: with no sink, one word test per span. With a sink, two clock calls, an emit under IRQ mask and a row update, about 40-60 instructions [inferred]. Budget: the no-sink cost stays within the 0.02% instruction-ratio A/B; the with-sink cost is reported as an `overhead` metric; no budget before the pilot (section 8).
- Naming: modules from the load table; services from the generated `[[service]]` order exported to `gen_proto`; hooks by wrapper symbol via the address map; polls as today.
- Output: `dk.profile/1` spans gain the kinds `service` and `hook`; `wb profile spans` and the Profile pane add "Services" and "Hooks" groups. Emulator and device use the same path (dk-session to the debug module), no `mem.cf.peek`.
- Host tests: runtime spans tests (hook and service owners, no overlap); debug host test (rows, class counts); dk-session fake; dkprof new kinds; profile-core grouping. Device test: one case in `debug-module.device.test.ts` on DN2 and DT2: a page key gives a hook row with calls > 0.

**P8d, DSP stats window (2 days).**

- Step 1, before the DSP macros: the window-3 read case in the DSP routine and its host test. The reply record (687 words; `docs/findings/31-dsp-link.md:86-124`, `78-reply-inflight.md`) has no free run of 7 words on either device (stock words, stock DSP rows, `dl_diag` in rows 1-16 at +0x44/+0x48, DN2 effect-bus audio in all rows), so the stats are not copied into the reply record.
- Measured on the SHARC: one stats row per DSP dispatch row (rows 0-7 machines, 8-15 effects [inferred: fx rows <= 8]). Per row: calls, cycles summed, largest call, largest block. Window v2 has 6 words per row and replaces v1 in `sdk/dsp/contract/dk_trace.inc` and `sdk/include/dk/core/trace.h` when P8d is built. Nothing implements v1. Header: blocks, largest stock block delta, overruns.
- Counted in image code only: `DK_SPAN_BEGIN/END` at the existing mark sites in `fx.s` and `dsp-voice*.s`; `_dk_span_add`, the window and the end-of-block fold in the dsp-link block tail. When stubs move to services (runtime-and-services 6.1), only the macro sites move.
- Cost: off, 3 instructions per macro (6 per call); on, about 20 per call plus a 16-row fold of about 64 per block [inferred]. On the emulator the EMUCLK read takes code out of the fast tiers; emulator figures are instruction counts, not cycles.
- Overrun: a block whose stock block delta (HEALTH store) is at least the block period in cycles, that is, a block at 100% of the period. The ColdFire writes the period. The UI also draws the 85% budget line. TPERIOD/TCOUNT and the core clock are not measured on hardware: [open] until P9.
- Transport: the stats live in a DSP DM window behind `_dk_stats_ptr` (`sdk/dsp/contract/dk_trace.inc:21-62`, `sdk/include/dk/core/trace.h:38-40`). The ColdFire reads it through dsp-link window 3, which becomes a read window. Today window 3 is refused (`modules/core/adapters/dsp-link/src/dsp/dsp-link.inc:28,181`); the comment "first window number no writer may use" in `modules/core/runtime/abi/src/link.rs:11-18` changes accordingly. It needs a new window case in the DSP routine (the `_dl_win2`/`_dl_wdone` code shows the mechanism) and costs one window read per poll on the ColdFire side. Two control words (on, clear generation) are written with `link::set`, only by the owner of the span channel. `dl_tick` keeps a ColdFire mirror (`dsp-link.c:253`). Runtime import `dk_dsp_stats_read` gives the mirror to the debug module. The new command `PROFILE_DSP` gets a new command id, with no renumbering; the `debug_profile` range grows (`gen_proto.ts:199`, sdk.toml). Link queue depth is ColdFire state: the pump keeps current and peak depth, returned by PROFILE_DSP [inferred: queue in dkrt-link].
- Host tests: Rust link (window 3 read-only, control words owner-only); dsp-link host test (window-3 read case, control words, fold); dk-session fake for PROFILE_DSP. Device test: one `dsp: 'sharc'` test, one machine and one effect: calls = voices x blocks, cycles > 0, zero when off. Golden PCM and hashes stay unchanged when off. Patched image hashes change, so `test:full` runs once for the batch.

**P8e, Profile pane DSP section (0.5 day).** A "DSP" section per module (rows mapped through `machine_types_of`, `handlers.rs:66`): cycles per block, largest call, largest block, calls per block; then overruns and link depth. The 85% budget line is drawn. Unit on the emulator: "instructions (EMUCLK)". The existing `sharc.profile` stays as the emulator-only per-function view. Host test: profile-core. Screenshot before handoff.

Payoff: per-module DSP numbers on the device, needed by P9 calibration and the broker. Confidence high for P8c, medium for P8d (the window-3 read case, SHARC register pressure at the mark sites).
Risks: the window-3 read case adds a DSP routine path and one read per ColdFire poll; the off cost changes the instruction counts `sharc_cells` sees; PROFILE_STATE format break; hooks with spans run in other tasks [inferred: page hooks in the UI task].

### 13.2 Item 9: device calibration (`wb profile calibrate`)

Not started. Order: after R12, P8c and P8d.

- Same counter on both sides: the P8d DSP stats rows read EMUCLK around each dispatch (`sdk/dsp/contract/dk_trace.inc:48-69`). On the device EMUCLK counts core cycles (SHARC+ PRM p.28-18). In the emulator it advances one per instruction plus modelled jump costs (finding 125, finding 103 section 8). Calibration is the device/emulator ratio per stats row.
- Model: one factor per device for module rows, fitted on worst-case statistics (largest call, largest block), not means. The budget factor is the worst observed device/emulator ratio in the calibration set, so it never underpredicts a measured case. Move to a per-class model (instructions plus weighted taken jumps and calls, from per-PC counts and the dksharc class of each PC) only if the spread exceeds 10%. Cache-set conflicts are reported as outliers, not modelled.
- Error per calibration: e_i = (f*insn_i - cyc_i)/cyc_i. Report n, mean, worst underprediction and worst leave-one-module-out. Each case is captured more than once (device block jitter is about +-2.7%, finding 103 section 8).
- Stock load and block size come from the device. Stock = the P8d header's largest stock block delta minus the module rows. Block cycles are measured (about 666,582 DT2 / 666,784 DN2, finding 103 sections 8 and 10). The absolute core clock is not needed; TPERIOD/TCOUNT are a cross-check. Two stock baselines are captured (a held note, a heavier project); the budget uses the higher.
- Data: device cycles come from P8d rows via PROFILE_DSP (the emulator answers the same command over the same dk-session and debug-module path). Instruction-class costs come from the dsp-facts probe rerun (TPERIOD, TCOUNT, MMASK, real dual MAC, SSFR). Rig per-PC counts are model input only, after one case proves that rig instructions per call equal the emulator row.
- Pairing key: {device, .dkm and DSP .dko sha256, case id and input hash, row}, with equal calls per block. Steady means and largest values are paired separately. A mismatch is refused with a reason. The dk.profile/1 `Run` gains a case id; the profile gains an additive `dsp_rows` field next to `spans`.
- Commands: `wb profile calibrate --device D.json --model E.json [...] --out cal.json`. The fit is in Rust (`dkprof::calibrate`, exported through dkprof.wasm), so wb, the workbench and patchkit run one code. File `dk.calibration/1`: key {device, firmware sha256, measured block cycles, instruction-count model: daemon sha256 or CORE_FINGERPRINT or rig core fingerprint}, factor {fit, budget, model}, error {n, mean, worst_under, loo_worst}, stock {avg, max}, pairs. `wb profile report --calibration cal.json` shows estimated cycles (+-e) and % of the measured block next to instructions. Instructions and cycles stay separate measures.
- Budget: the chosen file is committed as `devices/calibration/<dev>.json` and embedded like CYCLE_FACTORS (`packages/patchkit/src/budget_cli.rs`). Headroom = (pct*block_cycles - stock_cycles_max)/f_budget. It replaces `budget.rs:91-97`. The 85% margin stays until several devices and cases show a small error. Without a calibration file, today's factors (`devices/cycle-factors.json`, 1.13-1.41 from the manual's tables) apply.
- Calibration set: shot (memory-heavy), pluck (compute), analog (filters), tb303, bd (light), freqshift (FX).
- Without the device: `dkprof::calibrate` host tests on synthetic pairs (known factor recovered, underprediction flagged, unpaired and key-mismatched rows refused, leave-one-out); `report --calibration` and the patchkit budget against fakes. After P8d, an emulator-vs-emulator run must give exactly 1.000.
- With Em's go (the device restarts on upload): P8d capture on DT2 and DN2; the dsp-facts rerun.
- Steps: (1) calibrate crate, format, host tests (0.5 day, no P8d needed); (2) `dsp_rows` and case id in dk.profile/1 with P8d/P8e; (3) `wb profile calibrate` and `report --calibration` (0.5 day); (4) emulator captures of the set; (5) device captures (Em's go); (6) the patchkit budget switches to the calibration. Patched image hashes change, so `test:full` runs once for the batch.

## 14. Open items

- Answered by `packages/dksharc` (3.3): a module DSP section is analysed in Rust. The DSP load address per run is still open (3.2).
- Answered by item 1 (emulator): load addresses come from a PEEK of the runtime's module table `DKRT_STATE` (`mem.cf.peek`); no event field. On a device the same table is read by the debug module's PEEK (`dklog crash --image`); not run on hardware [O].
- Answered by P8d (13.1): the host reads the DSP stats window through the new command `PROFILE_DSP`.
- [O] The device transport (`dklog`, `dkupload`) has not run on hardware.
- Answered by the clock audit ([finding 125](../findings/125-dsp-cycle-counter-consumers.md), [D]): EMUCLK reads only report. One reader per image, the SHARC block handler; its low-word cycle delta feeds the HEALTH page through `core/health`. Nothing branches on it.
