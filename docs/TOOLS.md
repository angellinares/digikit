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
| `tools/ghidraq.py` | Run read-only PyGhidra queries for functions, callers, references, strings, ranges, and decompilation. Queries can be chained with `--then` so one JVM answers several questions. |
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
DAG address updates, memory accesses, and two independent delay slots.
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
| Which non-main instructions disagree with the decoder? | `sharc_seeddecode.py --only-problems` |
| Does a pointer remain `base + stride*index + offset`? | `sharc_trace.py` |
| Did Ghidra miss a ColdFire reference? | `refscan.py` |
| Did a language change regress decoding or analysis? | `sharcpcode.py compare` |
| Can a diagnostic string identify a subsystem? | `sharcstrings.py`, then require a real reference |
| Does an emulator observation hold across snapshots? | `snapdiff.py`, `memdump.py`, or a focused probe |

Record verified results in [`FINDINGS.md`](FINDINGS.md), not in this tool
guide. Keep current state and next steps in the newest handover.
