# Reverse-engineering tools

This repository includes small, purpose-built tools for extracting firmware,
querying Ghidra, measuring the emulator, and analysing the SHARC+ program.
This page is the index: use each tool's `--help` and module docstring for its
full command line.

## Generated-file rule

Firmware and anything derived from it must stay out of git. This includes
`.syx` files, extracted `sections/`, snapshots, Ghidra dumps, measurement
databases, disassembly reports, and mapped string output. Put repeatable
results under `out/`; commit the tool and synthetic tests, not its output.

Before trusting an emulator run, confirm that `sections/.source-sha256`
matches the source `.syx`. Bound emulator runs with an instruction limit.

The full test suite is:

```sh
uv run --with pytest python -m pytest tests -q
```

## Firmware and container tools

| Tool | Purpose |
| --- | --- |
| `python -m emu.extract` | Extract all sections from a supplied `.syx`. This is the normal entry point. |
| `tools/roundtrip.py` | Rebuild and re-extract a container as an acceptance gate for the repack chain. |
| `tools/content_hmac.py` | Inspect or calculate the container's content-authentication trailer. |
| `tools/patchimg.py` | Apply byte-exact, preconditioned patches to an extracted section image. |
| `tools/ddr_geometry.py` | Derive DDR geometry from the bootstrap's initialization sequence. |

Never patch the bootstrap or updater sections. See
[`PATCHING.md`](PATCHING.md) before producing an image intended for hardware.

## ColdFire static analysis and Ghidra

| Tool | Purpose |
| --- | --- |
| `tools/ghidradump.py` | Export an analysed Ghidra program into grep-friendly disassembly/decompilation plus SQLite indexes. |
| `tools/ghidraq.py` | Run read-only PyGhidra queries for functions, callers, references, strings, ranges, decompilation, and bounded HighFunction dataflow. Queries can be chained with `--then` so one JVM answers several questions. |
| `tools/refscan.py` | Exhaustively scan a raw ColdFire image for direct references into an address range. Use this to check Ghidra's incomplete reference tables. |
| `tools/codeseeds.py` | Recover likely function entries from vectors, calls, and code pointers. |
| `tools/ghidraapply.py` | Apply code seeds or RTTI discoveries to a Ghidra project. |
| `tools/rttiscan.py` | Locate GCC RTTI, vtables, and class/method strings in a ColdFire image. |
| `tools/ghidravt.py` | Run and export Ghidra Version Tracking matches between two programs. |
| `tools/vtcheck.py` | Check Version Tracking matches against the images and analysis dumps. |
| `tools/hwlookup.py` | Resolve an MCF5441x address, exception vector, or eDMA channel through the cited hardware-reference contract. |

`hwlookup.py` is backed by
`docs/contracts/mcf5441x-reference-v1.json`, not by guessed peripheral names.
For example, `hwlookup.py address 0xfc045654` reports TCD50.CITER,
`hwlookup.py dma 50` connects SSI0 TFE0 to vector 170, and
`hwlookup.py vector 191` reports the unassigned hardware source and its
`INTFRCH1` software-force bit. Register and routing facts retain manual-page
or independent-header provenance, and contract tests keep emulator constants
consistent with the lookup.
| `tools/export-profile.py` / `tools/apply-profile.py` | Export resolved firmware symbols and apply them as Ghidra labels. |
| `tools/entryhist.py` | Summarise discovered function entries over known code ranges. |

An empty Ghidra caller or reference list is not proof of absence. Confirm raw
references with `tools/refscan.py`, especially for trampolines and code outside
recognised functions.

Typical read-only query:

```sh
uv run python tools/ghidraq.py /section_3_MAIN_OS.bin callers 0xADDRESS \
  --project ~/ghidra-projects/dt2-emac --project-name dt2-emac
```

### Raw instruction p-code and bounded HighFunction dataflow

`pcode PC` reports raw `Instruction.getPcode()` at exactly one listed instruction,
without decompiling, changing references, or inferring SSA. Its JSON includes
requested, logical, and displayed coordinates, instruction length, ordered p-code
operations, and address/language metadata. `no-instruction`, empty, and malformed
p-code are structured statuses. It is a diagnostic, not an SSA slice.

`slice PC SELECTOR` finds the containing function and follows a bounded backward slice;
selectors include `store:value`, `store:address`, `load:value`, `load:address`,
`input:N`, `output`, and `reg:NAME`. `stores TARGET [LO HI]` and its symmetric
`loads TARGET [LO HI]` inspect memory writes and reads globally or for function
entries in `[LO, HI)`, reporting exact direct addresses separately from computed
or unresolved candidates. Raw `STORE`/`LOAD` and HighFunction direct-memory
`COPY` forms are normalized with `STORE`, `LOAD`, `COPY_DIRECT_WRITE`, or
`COPY_DIRECT_READ` representations. A `COPY` with direct memory on both ends is
the decompiler's folded memory-to-memory move: it reports as
`COPY_MEM_TO_MEM_WRITE` for `stores` and `COPY_MEM_TO_MEM_READ` for `loads`, so
one instruction is visible from both its destination and its source. Both use
the normal decompile timeout,
report failures/partial results instead of stopping a `--then` chain, and do
not save the project.

For the ColdFire program, raw/displayed addresses are unprefixed:

```sh
uv run python tools/ghidraq.py /section_3_MAIN_OS.bin pcode 0x40008000 \
  --json --project ~/ghidra-projects/dt2-emac --project-name dt2-emac
uv run python tools/ghidraq.py /section_3_MAIN_OS.bin slice 0x40008000 store:value \
  --json --project ~/ghidra-projects/dt2-emac --project-name dt2-emac
```

For SHARC, replace `SHARC_PROGRAM` and `SHARC_PROJECT` with the program path
and project containing that program. The program must be imported with
`SHARC_VISA:LE:32:default`; use `sw:` for a short-word PC target. DM data
targets (on-chip and external alike) are byte addresses and go unprefixed --
the generated language translates a DM byte address into the `ram` space's
unit offset internally (`tools/sharcspec/ghidra/gen_sleigh.py`
`dm_byte_addr_to_ram_unit`), so the plain literal from the instruction
stream is also the address to query here:

```sh
uv run python tools/ghidraq.py SHARC_PROGRAM pcode sw:0x1c1928 \
  --json --project SHARC_PROJECT --project-name SHARC_PROJECT
uv run python tools/ghidraq.py SHARC_PROGRAM loads 0x254d98 \
  --json --project SHARC_PROJECT --project-name SHARC_PROJECT
uv run python tools/ghidraq.py SHARC_PROGRAM stores 0x8055c874 \
  --json --project SHARC_PROJECT --project-name SHARC_PROJECT
```

Results identify p-code by sequence PC/time and structured varnode fields, not
Java display strings. SHARC_VISA results include byte/short-word metadata and
the caveat that HighFunction p-code does not model delay-slot execution.

## Emulator measurement tools

| Tool | Purpose |
| --- | --- |
| `tools/bootcheck.py` | Deterministically classify whether a bounded boot reached its expected milestones. |
| `tools/addrtrace.py` | Instrument the addresses used by `bootcheck.py`. |
| `tools/bootwatch.py` | Watch selected guest writes during cold boot. |
| `tools/memdump.py` / `tools/memfind.py` | Dump or search mapped memory after resuming a snapshot. |
| `tools/snapdiff.py` | Compare guest memory between snapshots. |
| `tools/mmiotrace.py` | Find MMIO addresses touched periodically by the running OS. |
| `tools/steptrace.py` | Explain the instruction accounting of repeated emulator `spin` calls. |
| `tools/panelsweep.py` | Map front-panel code through recorded queue events. |
| `tools/inputlag.py` | Measure latency from a panel event to firmware response. |
| `tools/guirun.py` | Reproduce the GUI worker configuration without opening the GUI; `--no-unblock` preserves real waits, `--intro-timers pit3` drives only the intro's PIT3 event, `--panel-raw-at` saves an existing latched frame, and `--block-profile` is explicitly perturbing. |
| `tools/experiment.py` | Run a fresh, exact repeated baseline versus one button gesture and save endpoint/diff reports. |
| `tools/uidrive.py` | Watch for and optionally drive the experimental machine-list UI path. |

A Phase 1 recipe is a deliberately narrow JSON experiment, not a general framework.
It requires a hash-consistent `DT2_SECTIONS`/`DT2_SYX` pair and never reuses a
run ID; all firmware-derived logs, endpoints, and RAM diffs go under ignored
`out/experiments/`. The requested save count is a lower bound: reports record
both requested and actual saved instruction counts, and compare cases only at
the same deterministic actual endpoint at or after that bound. A manipulated
run is successful only when both paced input batches (press and release) were
actually delivered before the endpoint:

```sh
uv run python tools/experiment.py experiments/phase1-button.json --run-id YYYYMMDD-button
```

`experiments/a1-func-src.json` is the narrow FUNC/SRC raw-feed A1 recipe. It
runs quarantined state and profile lanes: state snapshots/panel frames are the
state evidence, while the profile lane's UiTrace and **perturbing** basic-block
entry profile are only scoped dynamic call/view and block-entry evidence, not
instruction coverage or state claims. Invoke it the same way:

```sh
uv run python tools/experiment.py experiments/a1-func-src.json --run-id YYYYMMDD-a1
```

The A1 panel latch time is the live timer clock relative to the resumed run,
so it can be compared with that run's observation save.  The manipulated
profile gate follows the documented low-backlog trajectory: SRC chord press,
MachineSelectionView activation, suppressed SRC release/repeat and no view
close, then FUNC release.  Exact delivery of the raw SRC-up feed is checked
separately.  Profile JSON must repeat byte-for-byte within a case; text logs
need not, because they contain wall-clock rates.

For DT2 1.16, a faithful post-intro run uses
`--exact --no-unblock --intro-timers pit3` from the intro snapshot, then
preserves timer, UART eDMA,
and eSDHC/card state in each checkpoint. `--intro-timers pit3` is deliberately
narrow: PIT3 releases the intro's real frame semaphore, DTIM remains held, and
the normal PIT channel set is restored at the intro handover. It is not a
generic semaphore bypass.

### Opt-in SSI0/eDMA event source

`tools/guirun.py --ssi0-request-hz N` enables the narrow SSI0 model in
`emu/ssi.py`. It advances only the observed eDMA48 receive and eDMA50
transmit descriptors, including 32-byte minor loops, 64-minor major loops,
32-byte scatter/gather reloads, and eDMA50's vector 170. The guest vector-170
ISR remains responsible for `EDMA_CINT=50` and `INTFRCH1[31]`; the model
delivers vector 191 only from the image-resolved RTE boundary, never directly
from eDMA completion. RX destination bytes are preserved: the external serial
peer's data is unknown and is not fabricated.

There is intentionally no default for `N`. The captured 1.16 state selects
external SSI bit clock and frame sync, so the board's request cadence cannot
be derived from the SoC divider registers. A chosen value is an explicit
exploration profile, not a verified hardware rate. Add the model to a legacy
checkpoint exactly once:

```sh
uv run python tools/guirun.py OLD.snap --exact --no-unblock \
  --ssi0-request-hz N --ssi0-upgrade-legacy \
  --save-at COUNT:SSI.snap
```

Resume `SSI.snap` with the same `--ssi0-request-hz N` and without
`--ssi0-upgrade-legacy`. The upgrade validates the live TCD48/TCD50 peripheral
ends, starts a fresh SSI event clock at the checkpoint boundary, and writes a
separate `ssi0_dma` component plus manifest feature. Old snapshots never
silently acquire it, and a different request rate is rejected. Exact SSI
runs must use `--exact`; the fast stepper remains discovery-only.

The bounded no-payload control under
`out/experiments/ssi0-dma/control-001/` reaches the generic vector-170 handler
16 times in 400,140 instructions with zero faults; clean and traced endpoint
snapshots are byte-identical. It does not hand over to the normal handler,
because all candidate RX marker rows in the source checkpoint are zero and
the model correctly does not invent the external `0x007fffff` sync word. The
separate `vector-chain-control-001` host-patches only the vector slot and is
calibration evidence, not behavioral evidence.

`tools/guirun.py --poke ADDR=LONG` applies a repeatable big-endian 32-bit
guest-memory write after snapshot restore and before execution. The run log
labels every such write as a host calibration. Use it only for controls: a
poke is never firmware or panel provenance.

### Exact-run throughput

Use immutable snapshots and parallel processes for independent repetitions.
The current-host benchmark at
`out/benchmarks/exact-workers-1.16-001/report.json` ran identical 20M-counted-
instruction DT2 1.16 jobs with `--exact --no-unblock`. One, two, four, and
eight workers completed in 10.245, 10.360, 10.577, and 10.575 seconds,
respectively, for aggregate rates of 1.962, 3.880, 7.601, and 15.207 MIPS.
The recorded panel and endpoint-snapshot hash pair is the same for all 15
runs. Eight workers are therefore the measured throughput choice on this
host; benchmark again on a different machine rather than treating eight as a
portable constant.

The shorter qualified machine-selection schedule is recorded at
`out/experiments/panel-machine-commit/qualify-1.16-fast-001/report.json`. It
keeps the timing needed for FUNC+SRC and the DOWN selection but advances YES
from 52.4M to 46.4M requested instructions and ends at 64M instead of 80M.
Two clean and two traced runs have byte-identical endpoints and the same panel
hash as the original qualification; the traced runs still record exactly one
type-2 commit and setter call. The commands contain no `--weakptr`, whose
default is false. This is the preferred schedule for repeating that
experiment. It shortens the scenario; it does not make one emulated
instruction execute faster.

The same schedule's setter-notification qualification is recorded at
`out/experiments/a2-notification-invalidation/qualify-001/report.json`. Four
independent processes restore the same immutable checkpoint: two clean and
two traced. The traced lanes hook only the commit, setter,
`ValueWithMirror` notification, `Sound::updateMirror`, registered dispatcher,
unconditional invalidate and refresh, plus the selected source/cache/row
bytes. All four endpoint snapshots and panels are byte-identical. The traced
runs prove real panel -> one setter -> null-info dispatcher ->
`FUN_4002da38(track 0)` -> cache slot `0x8000470c` zero. They also record zero
`FUN_4002d438` hits and no row-type write, so this qualifies only the
notification/invalidation front half, not behavioral A2. Eight downstream
observer hits are not eight setter calls.

Do not use translation-block `icount` as exact executed-instruction
accounting. The throwaway probe at `out/prototypes/tb-stepper/` combined whole
TBs with a counted deadline tail. It was only 1.30x faster over 5M requested
instructions and diverged from the counted oracle in six registers, the FF1
counter, and 137 decompressed bytes across four pages. PC-redirection hooks
make translated block length an unsafe proxy for executed length; the probe
did not isolate which hook caused the first divergence. The existing
approximate fast stepper remains discovery-only, and `--exact` remains
required for qualification.

For retryable debugging, prefer a fresh process restored from an immutable
`--save-at` checkpoint, then combine `--at`, `--stack-at`, `--dump-at`, and
`--watch`. A checkpoint includes the host-side timer, UART eDMA, and eSDHC
state that a raw Unicorn CPU/memory context does not. An external Unicorn
debug server can still be useful for manual discovery, but it does not replace
this checkpoint state, accelerate counted runs, or qualify an experiment.

## Machine and frame-link tools

| Tool | Purpose |
| --- | --- |
| `tools/machineprofile.py` | Identify a known MAIN OS by hash and report its machine-related anchors and preconditions. |
| `tools/machinecommit.py` | Run the bounded DT2 1.16 direct source-object -> SRAM row -> TX-frame A/B experiment. |
| `tools/machinepatch.py` | Experimental, build-specific live relocation of the UI machine table. |
| `tools/framelink.py` | Describe the ColdFire-to-SHARC frame layout for known images. |
| `tools/sharcframe.py` | Capture the frame sent by the ColdFire from an emulator snapshot. |
| `tools/dspmap.py` | Find ColdFire instructions that reference frame-link tables. |

`machineprofile.py` is the guardrail for the patching tools: do not carry an
address from one firmware image into another without a matching profile and
byte preconditions.

`machinecommit.py` accepts only the known Digitakt II 1.16 MAIN image. It
restores the same snapshot fresh for baseline, source-only,
unchanged-refresh and changed-refresh conditions, each clean and with bounded
refresh/row observers. It fails unless the active direct-refresh controls,
three 2050-byte frame passes, one-cycle type result and clean/instrumented
equivalence all hold. The experiment proves the direct
`source + 0xa2 -> FUN_4002d438 -> row -> frame` back half only; it does not
prove the setter notification or cache-invalidation front half.

```sh
DT2_SECTIONS=out/sections/dt2-1.16 \
DT2_SYX=Digitakt_II_OS1.16.syx \
uv run python tools/machinecommit.py \
  out/snapshots/dt2-1.16/boot400M.snap \
  --image out/sections/dt2-1.16/section_3_MAIN_OS.bin \
  --track 0 --type 5 --passes 3 --limit 5000000 \
  --json out/experiments/a2-machine-provenance/RUN-ID/report.json
```

## SHARC+ loader and memory map

The DSP firmware is a loader stream containing multiple writes to mapped
memory, not one flat executable. The address mapping and last-write order are
important.

| Tool | Purpose |
| --- | --- |
| `tools/sharcldr.py` | Parse loader records, validate headers, map file offsets to loaded addresses, dump selected blocks, and extract the final main-program region. |
| `tools/sharc_import.py` | Import the complete loader memory map into Ghidra, replaying writes in stream order. |
| `tools/sharcscan.py` | Recover direct calls and candidate dispatch tables across the loader stream. |
| `tools/sharcstrings.py` | Map bounded printable runs to their loader blocks and loaded addresses, with optional SQLite-reference correlation. This is an orientation aid, not semantic evidence. |

Examples:

```sh
uv run python tools/sharcldr.py out/sections/dt2-1.16/section_7_BLOB.bin --align

uv run python tools/sharcstrings.py \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  --grep 'frame|buffer' --limit 100 --json
```

Mapped-string JSON is firmware-derived and belongs under `out/`, never in a
commit. Printable instruction bytes also look like strings; require a real
data reference before treating a hit as a label.

## SHARC+ decoding and control flow

| Tool | Purpose |
| --- | --- |
| `tools/sharc_disasm.py` | Decode variable-width VISA instructions from the public-manual-derived tables. Unknown or ambiguous instructions stop rather than silently desynchronising. |
| `tools/sharcflow.py` | Recover delayed calls and returns, measure coverage, and optionally seed/repair Ghidra functions. |
| `tools/sharcimm.py` | Search decoded instruction fields, or every loaded block's words, for immediate values and peripheral addresses. |
| `tools/sharcfields.py` | Report per-form decoded-field distributions across main programs. |
| `tools/sharccompare.py` | Compare the generated decoder against the independent specification decoder. |
| `tools/sharcpcode.py` | Build and measure the generated Ghidra language, record decoder/Ghidra views in SQLite, and compare two measurement runs. |
| `tools/sharc_seeddecode.py` | Decode exact Ghidra instruction-start seeds from every loader-mapped region and report form, field, length, and confidence agreement without sweeping data blocks. |

Language changes should be measured before and after:

```sh
uv run python tools/sharcpcode.py measure --out out/sharcpcode/new --ghidra
uv run python tools/sharcpcode.py compare out/sharcpcode/old out/sharcpcode/new
```

Use `tools/sharcpcode.sql` and the generated SQLite files for questions the
measurement already records; avoid starting another Ghidra JVM just to repeat
a query.

To inspect a non-main function through the loader map:

```sh
uv run python tools/sharc_seeddecode.py \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  out/sharcpcode/new/dt2-1.16.sqlite \
  --function 0xFUNCTION --only-problems
```

The SQLite instruction starts are boundary evidence; the tool never performs
a linear sweep over loader regions that may contain data.

## Targeted SHARC+ data flow

These tools are deliberately narrow. They are faster and safer than pretending
the incomplete SHARC+ language can decompile an entire receive path.

### `tools/sharc_trace.py`

A bounded, delay-aware abstract interpreter starting at an exact short-word
PC. It currently models a proved subset of register moves, integer compute,
DAG address updates, direct Type 14a DM/PM transfers, other memory accesses,
and two independent delay slots. A normal Type 14a event marks that an
unmodelled SIMD companion access is possible; forced-long-word Type 14a stops
explicitly rather than pretending its neighboring register pair is one UREG.
Unsupported or provisional forms stop the state explicitly.

The default mode reads a flat extracted region and therefore requires
`--base-sw`. `--blob` instead decodes exact PCs directly from the complete
section-7 loader memory map, including non-main and cross-block code.

Seeds can be concrete or symbolic:

```sh
uv run python tools/sharc_trace.py out/sharc/dt2-1.16-main.bin \
  --base-sw 0x1c1338 --start 0xSTART \
  --set I2=@receive_words --set R4=@track_index \
  --max-steps 100 --max-states 32 --json

uv run python tools/sharc_trace.py \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  --blob --start 0xSTART --max-steps 100 --json
```

Symbolic arithmetic is affine, so expressions such as
`receive_buffer + 0x94 + 2*track_index` survive copies, addition, subtraction,
and multiplication by a constant. Non-affine operations become `Unknown`.

The direct-transfer support is sufficient to recover the concrete DAI0/DAI1
MMR stores in the 1.15C and 1.16 setup blocks. For example:

```sh
uv run python tools/sharc_trace.py \
  out/sections/dt2-1.16/section_7_BLOB.bin \
  --blob --start 0x1cb28b --max-steps 100 --json
```

This traces the primary PE through the DAI stores and then stops at the next
unsupported `9b_abs` form; it is not a general SHARC emulator.

### `tools/sharcemu.py`

A Ghidra p-code emulator harness (`EmulatorHelper`) for the SHARC+ program,
complementary to `sharc_trace.py`'s abstract interpreter: this one actually
runs the generated language's p-code, one instruction at a time, against a
live emulator register/memory state. It fails loudly instead of silently
no-opping: the generated SLEIGH emits an empty body for any VISA form it has
not been given semantics for (only ~22% of forms have any), so an
instruction with zero p-code ops executes as a silent no-op unless caught
first. Before every step the tool reads the instruction's raw bytes and
classifies them independently with the repo's own decoder
(`tools/sharc_disasm.py`, `tools/sharcinv.py`'s `merge_fields`), never
Ghidra's mnemonic. Only `Type21a` (an architectural NOP with no fields) and
`Type9a_abs`/`Type9b_abs` with merged field `b==1` (register-indirect call,
no static target to encode) are legitimately empty; anything else with empty
p-code is a `no-semantics` fault. A conditional jump's compute-condition is
an unimplemented CALLOTHER pcodeop in this language, so `step()` returning
false surfaces as an `emulator-error` fault carrying Ghidra's message.

```sh
uv run python tools/sharcemu.py dt2-1.16_SHARC --start sw:0x1c18ed --steps 200 \
    --watch 0x254d9c --set R2=0x1234 \
    --project ~/ghidra-projects/sharc-batch-dt2-116 \
    --project-name sharc-batch-dt2-116 --json
```

`--start` takes `sw:` (short-word) or a displayed coordinate for the program
counter, same convention as `ghidraq.py`. `--watch` and `--poke` addresses
are always plain DM byte addresses, unprefixed: `DM(0x254d9c)` is emulated
at `0x254d9c` directly, matching the loader's placement of on-chip and
external DM literals at their own byte address. The generated language
translates a DM byte address into the `ram` space's unit offset internally
(`tools/sharcspec/ghidra/gen_sleigh.py` `dm_byte_addr_to_ram_unit`), so the
emulator now sees the image's initialised data at these locations. See
`docs/findings/05-sharc-isa-and-decoding.md`.
`--watch ADDR[:LEN]` (repeatable, LEN in bytes, default 4) reports every
write touching that range: step, PC, address, and old/new bytes. `--set
REG=VALUE` seeds a register before `--start` runs; `--poke ADDR=VALUE` seeds
a 32-bit little-endian DM word first, for when the register you want to seed
gets reloaded from memory before reaching the code you want to watch.
`--skip-faults N` records up to N faults and advances past them instead of
stopping at the first one, to harvest a fault table.

Write detection prefers `EmulatorHelper.enableMemoryWriteTracking()` /
`getTrackedMemoryWriteSet()` (present in Ghidra 12.1.3), which reports an
address as written regardless of value, catching a write that happens to
store the same value already there; a before/after `readMemory` diff is the
fallback only if that API is absent, and cannot see a same-value write.
`getTrackedMemoryWriteSet()` returns a reference to EmulatorHelper's own
live, mutating set rather than a snapshot, so the tool copies it into an
independent `AddressSet` each step before diffing -- aliasing it instead
silently loses every write after the first step.

### `tools/sharc_worklist.py`

Measures how much of a SHARC+ region the current language gives real p-code.
It sweeps the aligned instruction stream (`tools/sharcflow.py` `aligned()`),
lifts each instruction with the in-tree language (`tools/sharcpcode.py`
`load_context`/`lift_one`), and tabulates, per form and overall, the share
that lifts to at least one p-code op. Type21a, and Type9a/9b_abs with
`b == 1`, count as covered because empty is their correct semantics.

```sh
uv run python tools/sharc_worklist.py --image dt2-1.16 \
    --function 0x1c18a6:0x1c1f7d \
    --out-json out/sharc-semantics/worklist.json \
    --out-md out/sharc-semantics/worklist.md
```

`--function LO:HI` (short-word addresses, end exclusive) adds the same table
for one span. The output is a ranked worklist of forms still without
semantics.

### `tools/sharc_candidates.py`

Ranks exact Type19a address-adjust hypotheses in a `sharcpcode` SQLite file.
It checks byte and word forms of an offset, finds the nearest source writer,
and classifies the adjusted index register as consumed, overwritten, or
blocked by uncertain/control flow.

```sh
uv run python tools/sharc_candidates.py \
  out/sharcpcode/new/dt2-1.16.sqlite \
  --offset 0x94 --word-bytes 2 --window 64 --json
```

A candidate is a hypothesis, not a finding. Confirm its base-pointer
provenance and the consuming memory operation with the tracer and image bytes.

### `tools/sharcwriters.py`

Which SHARC+ instructions can write a given DM byte address -- image-wide,
over every store form, not just the ones Ghidra's SLEIGH language can see.

Census: decodes every aligned instruction in the code blocks
(`tools/sharcinv.py` `CODE_BLOCKS`) and finds every DM store from decoded
fields alone (no SLEIGH, no execution): forms 15a/15b/3a/3b/3c/3d/4a/4b/4d/
6a_mem/14a/14d store when merged `d==1`/`g==0`; 16a/16b always store, DM
when `g==0`; dual-memory 1a/1b store on the DM side when `dmd==1`.
(`sharcinv.py`'s own `MEM_FORMS` omits Type3c even though it has a real `d`
bit and stores exactly like its siblings -- this tool keeps its own table.)

Resolve: groups the census by owning function (`tools/sharcfn.py`
`load_context`/`build_inventory`) and runs `tools/sharc_trace.py`'s tracer
once per function from its entry, with every I/M/B register seeded as its
own named symbol (so an address survives as `Affine(I6e - 12)` instead of
collapsing to `Unknown`) and `concrete_memory=True`. L registers are seeded
concrete zero rather than symbolic -- the one deliberate exception, because
the tracer's own Type19a_scaled MODIFY handler only takes its cheap linear
path for a `Const` zero L register; a merely-symbolic L instead poisons
every later use of that I register with `Unknown`, which was the single
largest source of lost resolution on a full run.

Classify: `HIT` / `EXCLUDED-CONST` / `EXCLUDED-STACK` / `STACK-RELATIVE` /
`LOADED-POINTER` (address depends on an unresolved memory load; the load's
own expression is recorded) / `ENTRY-RELATIVE` (depends on a
caller-supplied register; every register in the expression is listed,
including a mixed frame+modifier idiom like `DM(I7,M7)`) / `UNRESOLVED`.
Every census DM store lands in exactly one class; the class totals always
sum to the census total (checked at the end of every run).

```sh
uv run python tools/sharcwriters.py 0x252658 --jobs 16 \
    --json out/sharcwriters/252658.json
```

`--stack LO:HI` overrides the default stack bounds (derived from the
loader's own layout: the untouched gap between two code blocks, DM
`0x26f000`-`0x2c0000` -- see the module docstring); `--stack none` reports
every stack-relative store as `STACK-RELATIVE` instead of trying to exclude
it. `--jobs N` runs one function's trace per worker process. The
interpreter-independent logic (census rules, width tables, the address
classifier) is pure and unit-tested in `tests/test_sharcwriters.py` with
synthetic fields and trace events -- no firmware required.

`out/sharcwriters/stack-invariant.md`/`.json` census every instruction in
the image that can write I6, I7, B6 or B7 (7764 instances, whole-image,
function-owned) and argue S = `[0x26f000, 0x2c0000)` (the same
`DEFAULT_STACK_LO`/`HI`) usually contains them -- **conditionally, not
proven closed**: the census traces I7 through a genuine multi-context
stack-switch shape (`blk69@0xb8853a` sw `0xb885f5`, `I7` repointed to a
runtime-populated context-struct address) that cannot be resolved
statically to either "still in S" or a specific other region. See the
proof's "Item 2" for the full writer-by-writer table and verdict. Every
`EXCLUDED-STACK` result now states this explicitly rather than hiding it:
an `'assumption'` string (`ENTRY_SEED_ASSUMPTION`) on every row, plus
`excluded_stack_depends_on_unproven_entry_assumption` (a count -- every
`EXCLUDED-STACK` row, since the assumption underlies `STACK_SYMBOLS`
itself) in the JSON's top level.

The proof's one classifier-relevant fix lives mostly in
`tools/sharc_trace.py`: the Type19a_scaled circular-MODIFY handler
previously collapsed to `Unknown('scaled circular modify I%d')` whenever
the pre-modify value, B or L were not all concrete -- true on nearly every
occurrence, since I6/I7 are seeded as named symbols at each function's own
entry. PRM p.6-7 guarantees the wrapped result still lands in
`[B,B+L*scale)` regardless, the *same* window a value already bounded by
S (an entry-time symbol, or an earlier circular-MODIFY result) already
denotes -- so `_stack_bounded_symbol()` recognises that shape and the
handler mints a **fresh**, uniquely-named `circ_<reg>_<pc>` symbol for the
result. Fresh, not reused: an earlier version of this fix re-used the
input symbol unchanged, which asserted two different circular MODIFYs of
I7 (different sites, or the same site with different runtime state) were
*equal*, letting the Affine algebra cancel their difference to a spurious
0 and alias two different stack frames -- replaced before it reached any
other tool (grep the image for `scaled circular modify` if adding a new
consumer of this reason string). `tools/sharcwriters.py`'s classifier was
extended to match: `is_circ_symbol()`/`combined_affine_range()` treat a
`circ_` term as ranging over S widened by `CIRC_WRAP_SLACK` (`L7*4 =
0x7f4`) on both sides -- not S itself -- and `via_circular_modify` marks
an `EXCLUDED-STACK` result that used this. Recovers 948 of the ~1045
stores the stack pointer's circular MODIFY was blocking for target
`0x252658` (`3441 -> 4389 EXCLUDED-STACK`, `8438 -> 7492 UNRESOLVED`,
`HIT`/`EXCLUDED-CONST`/`ENTRY-RELATIVE` counts unchanged); the remainder
needs an offset at or beyond one buffer length, or an unrecognised symbol
name, which the fix deliberately does not claim bounded (see the proof).

## Reference manuals

`tools/refstext.py` extracts supplied public manuals into searchable,
page-addressable text under `out/refs/`; `--render PDF PAGE` creates a PNG for
figures. Read the extracted text rather than repeatedly processing PDFs.

Only public SHARC+ references belong in documentation and generated language
sources.

## Choosing the fastest tool

| Question | Start with |
| --- | --- |
| What does this loader offset become in DSP memory? | `sharcldr.py` |
| Where is a known constant or peripheral address used? | `sharcimm.py`, then SQLite |
| Which exact offset calculations are plausible readers? | `sharc_candidates.py` |
| Which instructions can write a given DM address, image-wide? | `sharcwriters.py` |
| Which non-main instructions disagree with the decoder? | `sharc_seeddecode.py --only-problems` |
| Does a pointer remain `base + stride*index + offset`? | `sharc_trace.py` |
| Did Ghidra miss a ColdFire reference? | `refscan.py` |
| Did a language change regress decoding or analysis? | `sharcpcode.py compare` |
| Can a diagnostic string identify a subsystem? | `sharcstrings.py`, then require a real reference |
| Does an emulator observation hold across snapshots? | `snapdiff.py`, `memdump.py`, or a focused probe |

Record verified results in [`FINDINGS.md`](FINDINGS.md), not in this tool
guide. Keep current state and next steps in the newest handover.
