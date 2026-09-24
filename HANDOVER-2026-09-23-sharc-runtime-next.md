# Handover 2026-09-23: executable DSP path, hook search, emulator speed

This supersedes `HANDOVER-2026-09-23-sharc-parallel-next.md` for **next steps**.
That older, currently untracked handover describes the pre-Type7a frontier
and a now-blocked Type6a-first implementation plan. Durable evidence is in
`docs/findings/` (index: `docs/FINDINGS.md`); this file is an execution guide,
not a replacement findings document. Read `CLAUDE.md` before acting.

## Why we are doing this

The eventual goal is to understand and disassemble the SHARC+ DSP firmware
for Digitakt II and Digitone II, execute relevant DSP code in our emulator,
and **author and safely patch project-owned DSP code for custom machines**.
The prerequisite chain is: byte-accurate loader/ISA/disassembly -> trustworthy
semantics and a running DSP kernel -> actual machine/voice/audio-output path
and qualified hook -> assembly, loader-image integration, offline validation,
and only then a hardware experiment. A lower static stop count, a Ghidra
function label, a forced selector, or a ColdFire UI machine entry is not an
audio hook or proof that custom DSP code can run.

**Physical DT2 version needs confirmation before hardware work.**
`CLAUDE.md:5` says the device stays on 1.15C because installing 1.16 changes
its bootstrap irreversibly; the newer `README.md:18` says Em's device was
upgraded to 1.16 on 2026-09-20. These cannot both describe its present
state. Follow the conservative project rule (no device upgrade, no assumed
1.16 hardware observation) until Em confirms the installed OS/bootstrap.
We analyze DT2 1.16 offline regardless. For a hardware observation, first
match the actual device firmware to the analyzed bytes; if it is 1.15C,
byte-check the cross-version site mapping. No patched image from this repo
has been flashed to a real device. DN2 1.11/1.10E are comparison material,
not proof of runtime identity on DT2.

## Checkout and provenance at handoff

- Branch `work/machines-and-dsp-hooks`, HEAD `2be7ad3`. **Do not commit**
  unless Em asks. At last check, modified tracked files were
  `docs/findings/07-emulator.md` (new bounded speed result) and
  `docs/findings/10-sharc-indirect-target-dossier.md` (Type9b follow-up).
  The *previous* `HANDOVER-2026-09-23-sharc-parallel-next.md` was already
  untracked before this handover; this new handover is also untracked.
- DT2 1.16 `.syx` SHA-256:
  `278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec`.
  It matched both `out/sections/dt2-1.16/.source-sha256` and
  `sections/.source-sha256` here. Loader-final DT2 1.16 section-7 blob SHA-256:
  `0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`.
  Verify **again** before trusting a new trace/emulator run; do not confuse
  the `.syx` hash with the section-7 hash.
- Current ignored post-Type7a v5 writer index:
  `out/sharc-index/type7a-after-local-06adc1b.sqlite` (1,059 function facts).
  Older caches and counts in the previous handover are not the current
  frontier. The SHARC Ghidra dump lacks image binding and is advisory only.
- The last full suite after the Type7a implementation was **880 passed,
  7 skipped**; it was *not rerun* for the later documentation/benchmark work.
  `git diff --check` passed on the current documentation changes. No Type9b
  implementation or production emulator-speed optimization was made.

## What works, and what does not

- We have the loader/parser, typed public-manual-derived SHARC+ ISA
  (`tools/sharc_isa.py`, `tools/sharcspec/`), raw decoder, Ghidra language,
  cross-image function inventories, bounded fail-closed native tracer
  (`tools/sharc_trace.py`), incremental writer-function cache, and a separate
  Ghidra-free pypcode execution prototype (`tools/sharcemu.py`). See findings
  05, 06, 11, and 12. The last *documented* generated-p-code coverage was
  51.02% of aligned DT2 1.16 instructions after indexed DM forms were added;
  that is **not** 51% of a faithful audio emulator. Unsupported semantics and
  hardware behavior must fault/stop, never silently execute as NOPs.
- The ColdFire/Unicorn emulator boots the main OS and exercises the UI,
  machine selection, and host-to-DSP parameter traffic. It **does not
  execute the SHARC or produce audio**. A ColdFire machine descriptor only
  assigns names/parameter meaning to fixed slots; adding one does not give
  the SHARC a new algorithm. See `README.md` and findings 02, 04, 06, 07.
- A six-stage wavetable-looking DSP pipeline under `FUN_1c71ec`, including
  the two-tap lookup around SW `0x1cbf07`, is an investigation target
  (finding 06); it is not yet a verified live machine-to-output chain or
  a safe hook. The output/SPORT-DMA edge and natural dispatch values remain
  unproven. Work toward one **executable, checked-output DSP slice** before
  attempting whole-chip DSP emulation or arbitrary code injection.

## The latest static frontier, not an implementation invitation

The Type7a empty-compute `IF NOT SV` subset, with mixed-path fail-closed
writer classification, reduced affected predicate-stop *function facts*
from 22 to 2 over the same 1,059 IDs. It yielded no new writer `HIT`.
Type6a is blocked by a byte-backed ShiftImm selector `0x04` without
established public immediate semantics and uncertain MODE1; do not write a
predicate-only fix. Type11a byte-backed stop `0x1c0701` has nonzero compute,
so it is not a compute-free return fixture. See findings 05, 10, 12 and the
commits preceding HEAD for details.

The **current** Type9b index has 65 distinct stopped *function facts*, not
65 distinct sites: 37 `I12/M13`, 31 `I13/M13`, 1 `I12/M14`, with group
overlap. The previous handover's 63 was from a pre-Type7a cache. Bounded
traces sampled sites `0x1c351a` (selector-indexed I12 table), `0x1c6579`
(per-frame orchestrator's M4-selected I12 table), and `0xb891b4` (shared
followed helper). Independent image-byte review verified that `0xb891b2`
copies `R1` into `I13` immediately before the latter indirect transfer.
The tracer already resolves Type9b I+M transfers *when values are known*;
these stops are value-provenance gaps, **not** proof of missing Type9b
arithmetic. Four bounded sample paths to the helper leave R1/I13 unknown;
neither its machine/audio role nor natural R6/M4 selection at `0x1c6579`
is proven. Forced R6 table probes do not establish runtime selection or a
hook. The exact counts, byte claims and caveats are in finding 10. Do not
start a speculative Type9b handler edit.

## ColdFire speed experiment: useful result, different priority

The repo already defaults to scoped ISA/MMIO hooks, fast block-bounded GUI
stepping, and opt-in bit-exact soft-float/Bitmap HLEs. `count=` exact stepping
costs much more, but is required for deterministic instruction-accounted
tests. Turning off HLE may remove Python callbacks while executing vastly
more guest instructions; `unblock` changes scheduler behavior, not merely
speed. Replacing Python's Unicorn binding with Rust would still use the same
Unicorn core and must also preserve our patched build, hooks, timers, and
snapshots. No Rust A/B exists. Faster ColdFire emulation helps UI/traffic
iteration, **not** the separate SHARC pypcode executor directly.

From the hash-matched DT2 1.16 `snapshots/dt2-1.16/boot400M.snap`, three
bounded 10M-instruction `tools/speedab.py` repeats measured exact median
**7.334 s / 1.364M guest instr/s** (same final PC and count each time).
Fast mode reported median **4.449 s / 2.473M estimated instr/s**, but
estimated 10,999,956 instructions and ended at another PC: this is **not**
identical-work speedup. A separate instrumented exact profile attributed
45.0% to Python Unicorn binding, 35.2% native `emu_start`, 14.2% project
handlers, 5.6% other. Those instrumented shares are not direct time savings
on the uninstrumented timing run. The exact census counted 717,756 scoped
code-hook firings, 459,008 of them Bitmap get/set. Details and evidence
class are in finding 07; ignored JSON outputs under `out/speedab/` are
`dt2-116-{exact10m,fast10m,crossings10m,profile10m}.json`.

**Important failed fixture:** `tools/speedab.py` has no event injection.
An ignored one-off attempt in `out/speedab/prepare_traffic.py` made
`out/speedab/dt2-116-machine6-ready.snap` after reaching the UI and sending
16 stock machine-selection messages. The separate forced vector-191 TX-frame
capture did not return cleanly (one driver call, zero frame words). The
ignored `out/speedab/bench_traffic.py` required a timer-bearing checkpoint
to be restored via `deferred_components=('timers',)` and
`ev['restore_checkpoint_timers']()`; ordinary `tools/emucheck.setup()` does
not claim such a saved timer component. From this checkpoint, encoder `2:+30`
made no observable mirror/panel change; `2:-30` showed only a transient
panel difference at 2M, gone by 5M, and no change in any of the 16 raw
track-mirror rows. **This is NOT a validated parameter-traffic benchmark.**
Do not compare its exploratory timing with `speedab` or call the boot-window
numbers machine-traffic throughput. Investigate the frame/parameter evidence
first if ColdFire performance becomes the priority (finding 07).

## Recommended next work, in order

1. **DSP execution vertical slice (primary).** Choose one bounded kernel
   from the byte-identified wavetable pipeline, perhaps the stage-6 lookup
   around SW `0x1cbf07`. Recheck the DT2 1.16 blob hash and real instruction
   boundaries; specify its registers, memory inputs/outputs and deterministic
   seed fixture. Execute it from real loader-final bytes in the Ghidra-free
   backend until a checked output changes with input. Add only manual-backed
   semantics that block that slice, with synthetic and real-byte tests;
   compare typed decode, native-tracer effects, generated p-code and concrete
   execution. An unsupported compute, mode, delay slot, peripheral or map is
   an explicit stop. **Exit criterion:** repeatable nontrivial output on
   several inputs, not just N successful steps or a p-code coverage bump.
2. **Trace that slice outward.** Establish an actual per-frame voice/machine
   dispatch edge and buffer consumed by output DMA/SPORT. Separate static
   candidate paths from runtime-selected paths; seek natural selector/target
   values only where they discriminate a hook. Resolve the physical-version
   contradiction above before any device observation; verify an installed
   image or byte-check cross-version equivalents as appropriate.
3. **Only after an executable path and qualified hook:** design a small
   project-owned SHARC program, independently assemble/round-trip/decode it,
   place it in a loader image with preconditions and integrity checks,
   exercise it offline, and plan an explicit rollback/recovery strategy
   before any hardware consideration. Never modify bootstrap/updater sections.
4. **If ColdFire speed gates this path:** repair the machine/encoder fixture
   until an input produces a persistent, checked parameter or TX-frame delta
   against an uninjected control. Profile that *same* bounded workload,
   then test one high-frequency crossing or small native HLE in isolation.
   Do not port the entire harness to Rust on a binding-time percentage alone.

## Working rules and commands

- Use read-only independent subagents for byte/manual, path and measurement
  checks when useful; one writer for shared edits. Serialize live Ghidra
  project access, cold index builds and heavy emulator timings; Em may run
  emulator commands in this tree. An empty Ghidra caller list is not a raw
  no-caller proof: check `tools/refscan.py`. Do not switch execution modes
  silently after a subagent infrastructure failure.
- Firmware, extracted sections, snapshots, databases, bench JSON and other
  derived artifacts are copyright-protected and ignored; **never commit**
  them. Never identify, copy or quote private DSP tooling; rely on the public
  references listed in `docs/sharc/SOURCES.md`. Findings belong in
  `docs/findings/`; independent agent byte/provenance review is required
  before a firmware finding is marked `[V]`.
- Use `uv run python`, not bare `python`. For static language changes,
  measure with `tools/sharcpcode.py measure --out DIR [--ghidra]` and
  `compare OLD NEW` (separate JVM per language). For firmware emulator runs,
  first hash the `.syx` against the matching sections marker, and bound the
  run (`--limit` where supported; `tools/speedab.py` instead uses a bounded
  `--instrs` PIT floor). Keep timing runs sequential and on a quiet machine;
  exact and fast modes do not necessarily execute the same instruction stream.
- Last full suite command:
  `uv run --with pytest python -m pytest tests -q`. Re-run it after any
  semantic source changes; for documentation only, at least run
  `git diff --check`. Do not commit without an explicit request.

## First actions for the next context

1. Run `git status --short --branch` and compare with the checkout state
   above; read `CLAUDE.md`, `docs/FINDINGS.md`, findings 05/06/07/10/11/12,
   and the relevant portions of `README.md`. The older handover's HEAD,
   counts and Type6a-first plan are stale. Do not assume the physical DT2 OS:
   `CLAUDE.md` and `README.md` conflict; ask Em before hardware-specific work.
2. Choose the **DSP kernel executable-slice** milestone unless Em explicitly
   prioritizes ColdFire performance. Inventory the exact real-byte kernel
   instructions and p-code/tracer/backend gaps *before* changing semantics.
3. Record the smallest failing checked-output fixture and choose exactly one
   manual-supported semantic subset. Keep generator/cache/decoder changes
   measured and fail-closed. Delegate independent evidence review, but apply
   accepted changes with one writer and do not overstate reachability.
4. If working on speed instead, the open task is fixture correctness,
   not a Rust port: reproduce the forced frame failure and establish a
   persistent input-caused parameter delta before optimizing. The ignored
   scripts are diagnostic leads, not approved fixtures.
